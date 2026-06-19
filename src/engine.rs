//! Agent-as-operator engine: owns the IPC job queue, the history of remote
//! commands run on behalf of an external operator (`helm exec`), and the
//! SQLite-backed persistence layer.
//!
//! This module is deliberately independent of the TUI / ratatui machinery so
//! that the `helm daemon` subcommand can run it headless. The TUI's `App`
//! holds an `Engine` and forwards its read-only state into the agent-tail
//! pane; the daemon constructs an `Engine` directly and ticks it in a loop.

use std::collections::VecDeque;
use std::sync::mpsc::Receiver;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::history::{HistoryStore, LineKind, LineRecord, RunSource};
use crate::ipc::protocol::{Event as IpcEvent, Request as IpcRequest};
use crate::ipc::server::Job;
use crate::ssh::{RunEvent, RunHandle, spawn_remote};

/// One completed (or in-flight) agent-initiated run. Survives across helm
/// restarts when the history store is attached.
#[derive(Debug, Clone)]
pub struct AgentHistoryEntry {
    pub alias: String,
    pub cmd: String,
    /// Monotonic capture time — used for duration_ms math when the run
    /// completes. Never persisted (Instant is process-local).
    pub started_at: Instant,
    /// Wall-clock unix seconds at run start — persisted to history.db
    /// and used to sort SQLite-loaded entries against in-memory ones.
    pub started_at_unix: i64,
    pub output: Vec<AgentOutputLine>,
    pub exit: Option<i32>,
    /// True for entries reconstructed from the history DB on startup. Lets
    /// the UI tag them visually and skip re-persisting.
    pub from_history: bool,
}

#[derive(Debug, Clone)]
pub enum AgentOutputLine {
    Out(String),
    Err(String),
    System(String),
}

/// In-flight agent execution. While `Some(_)`, the engine forwards each
/// RunEvent to both the corresponding history entry and the socket worker
/// via `response_tx`.
pub struct AgentExec {
    pub handle: RunHandle,
    pub response_tx: std::sync::mpsc::Sender<IpcEvent>,
}

pub struct Engine {
    pub jobs_rx: Option<Receiver<Job>>,
    pub agent_history: Vec<AgentHistoryEntry>,
    pub agent_active: Option<AgentExec>,
    pub agent_queue: VecDeque<Job>,
    pub history: Option<HistoryStore>,
    /// Remembered default alias from the most recent `ingest_jobs` call.
    /// Used so that back-to-back queue advancement after a Done/Error can
    /// resolve empty aliases without the caller re-passing it on every
    /// internal step. Daemon callers always pass `None`.
    last_default_alias: Option<String>,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            jobs_rx: None,
            agent_history: Vec::new(),
            agent_active: None,
            agent_queue: VecDeque::new(),
            history: None,
            last_default_alias: None,
        }
    }

    pub fn attach_jobs_rx(&mut self, rx: Receiver<Job>) {
        self.jobs_rx = Some(rx);
    }

    /// Attach a SQLite-backed history store and rehydrate `agent_history`
    /// with the most-recent N agent runs in chronological order. Older
    /// entries are at index 0 so the AgentTail's append-only render keeps
    /// new entries at the bottom.
    pub fn attach_history(&mut self, store: HistoryStore, load_limit: usize) {
        // Cap retained rows so the DB doesn't grow unbounded across sessions.
        if let Err(e) = store.prune_to(5000) {
            tracing::warn!("history: prune_to(5000) failed: {e}");
        }
        match store.recent_runs(Some(RunSource::Agent), load_limit) {
            Ok(mut runs) => {
                runs.reverse();
                for r in runs {
                    let lines = match store.lines_for(r.id) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!("history: lines_for({}) failed: {e}", r.id);
                            continue;
                        }
                    };
                    let output: Vec<AgentOutputLine> = lines
                        .into_iter()
                        .map(|l| match l.kind {
                            LineKind::Out => AgentOutputLine::Out(l.line),
                            LineKind::Err => AgentOutputLine::Err(l.line),
                            LineKind::System => AgentOutputLine::System(l.line),
                        })
                        .collect();
                    self.agent_history.push(AgentHistoryEntry {
                        alias: r.alias,
                        cmd: r.cmd,
                        started_at: Instant::now(),
                        started_at_unix: r.started_at_unix,
                        output,
                        exit: r.exit,
                        from_history: true,
                    });
                }
            }
            Err(e) => tracing::warn!("history: recent_runs failed: {e}"),
        }
        self.history = Some(store);
    }

    /// Drain any incoming IPC jobs into the queue, then kick off the next
    /// one if nothing is running. `default_alias` is consulted when a client
    /// submits `Exec` with an empty alias (typical TUI flow: implies "use
    /// the host the operator currently has selected"). Daemon callers pass
    /// `None`, in which case empty-alias Execs error out.
    pub fn ingest_jobs(&mut self, default_alias: Option<&str>) {
        self.last_default_alias = default_alias.map(|s| s.to_string());
        if let Some(rx) = self.jobs_rx.as_ref() {
            while let Ok(job) = rx.try_recv() {
                self.agent_queue.push_back(job);
            }
        }
        self.maybe_start_next_agent_job();
    }

    fn maybe_start_next_agent_job(&mut self) {
        if self.agent_active.is_some() {
            return;
        }
        let Some(job) = self.agent_queue.pop_front() else {
            return;
        };
        let (alias, cmd) = match job.request {
            IpcRequest::Exec { alias, cmd } => (alias, cmd),
            // Server handles Ping/Shutdown inline; they never reach the queue.
            _ => return,
        };
        let resolved = if alias.is_empty() {
            self.last_default_alias.clone()
        } else {
            Some(alias)
        };
        let Some(alias) = resolved else {
            let _ = job.response_tx.send(IpcEvent::Error {
                msg: "no alias provided and no host selected".into(),
            });
            let _ = job.response_tx.send(IpcEvent::Done { exit: 1 });
            return;
        };
        let mut entry = AgentHistoryEntry {
            alias: alias.clone(),
            cmd: cmd.clone(),
            started_at: Instant::now(),
            started_at_unix: now_unix(),
            output: vec![AgentOutputLine::System(format!("$ ssh {alias} '{cmd}'"))],
            exit: None,
            from_history: false,
        };
        // Bind the scrutinee so its temporaries (RunHandle fds, response_tx
        // Sender) drop at end of block, identical across editions 2021/2024.
        let spawned = spawn_remote(&alias, &cmd);
        match spawned {
            Ok(handle) => {
                self.agent_history.push(entry);
                self.agent_active = Some(AgentExec {
                    handle,
                    response_tx: job.response_tx,
                });
            }
            Err(e) => {
                let msg = format!("spawn failed: {e}");
                entry.output.push(AgentOutputLine::System(msg.clone()));
                entry.exit = Some(1);
                self.agent_history.push(entry);
                let _ = job.response_tx.send(IpcEvent::Error { msg });
                let _ = job.response_tx.send(IpcEvent::Done { exit: 1 });
            }
        }
    }

    pub fn ingest_agent_events(&mut self) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(active) = self.agent_active.as_ref() {
            loop {
                match active.handle.rx.try_recv() {
                    Ok(ev) => events.push(ev),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        for ev in events {
            self.apply_agent_event(ev);
        }
        if disconnected {
            if let Some(active) = self.agent_active.take() {
                if let Some(entry) = self.agent_history.last_mut()
                    && entry.exit.is_none()
                {
                    entry.exit = Some(-1);
                    entry
                        .output
                        .push(AgentOutputLine::System("(channel closed)".into()));
                }
                let _ = active.response_tx.send(IpcEvent::Done { exit: -1 });
            }
            self.maybe_start_next_agent_job();
        }
    }

    fn apply_agent_event(&mut self, ev: RunEvent) {
        let Some(active) = self.agent_active.as_ref() else {
            return;
        };
        let response_tx = active.response_tx.clone();
        let entry = match self.agent_history.last_mut() {
            Some(e) => e,
            None => return,
        };
        match ev {
            RunEvent::Out(l) => {
                let _ = response_tx.send(IpcEvent::Out { line: l.clone() });
                entry.output.push(AgentOutputLine::Out(l));
            }
            RunEvent::Err(l) => {
                let _ = response_tx.send(IpcEvent::Err { line: l.clone() });
                entry.output.push(AgentOutputLine::Err(l));
            }
            RunEvent::Partial(_) => {
                entry.output.push(AgentOutputLine::System(
                    "(password prompt detected — agent command paused; \
                     answer in TUI Runner if intended)"
                        .into(),
                ));
            }
            RunEvent::NeedPassword => {
                entry.output.push(AgentOutputLine::System(
                    "(password prompt — needs human in TUI)".into(),
                ));
            }
            RunEvent::Done(code) => {
                entry.exit = Some(code);
                entry
                    .output
                    .push(AgentOutputLine::System(format!("exit {code}")));
                let _ = response_tx.send(IpcEvent::Done { exit: code });
                self.agent_active = None;
                self.persist_last_agent_entry();
                self.maybe_start_next_agent_job();
            }
            RunEvent::Error(msg) => {
                let _ = response_tx.send(IpcEvent::Error { msg: msg.clone() });
                let _ = response_tx.send(IpcEvent::Done { exit: 1 });
                entry.exit = Some(1);
                entry
                    .output
                    .push(AgentOutputLine::System(format!("error: {msg}")));
                self.agent_active = None;
                self.persist_last_agent_entry();
                self.maybe_start_next_agent_job();
            }
        }
    }

    fn persist_last_agent_entry(&mut self) {
        let Some(store) = self.history.as_mut() else {
            return;
        };
        let Some(entry) = self.agent_history.last() else {
            return;
        };
        if entry.from_history {
            return;
        }
        let lines: Vec<LineRecord> = entry.output.iter().map(agent_line_to_record).collect();
        let duration_ms = i64::try_from(entry.started_at.elapsed().as_millis()).ok();
        if let Err(e) = store.insert_run(
            RunSource::Agent,
            &entry.alias,
            &entry.cmd,
            entry.started_at_unix,
            entry.exit,
            duration_ms,
            &lines,
        ) {
            tracing::warn!("history: insert_run(agent) failed: {e}");
        }
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn agent_line_to_record(l: &AgentOutputLine) -> LineRecord {
    match l {
        AgentOutputLine::Out(s) => LineRecord {
            kind: LineKind::Out,
            line: s.clone(),
        },
        AgentOutputLine::Err(s) => LineRecord {
            kind: LineKind::Err,
            line: s.clone(),
        },
        AgentOutputLine::System(s) => LineRecord {
            kind: LineKind::System,
            line: s.clone(),
        },
    }
}
