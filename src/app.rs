use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::mpsc::Receiver;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::config::{builtin_logs, Config, Host, Log};
use crate::history::{HistoryStore, LineKind, LineRecord, RunSource};
use crate::inventory::health::{self, Health, HealthResult};
use crate::inventory::ports::{self as ports_inv, ListeningSocket};
use crate::inventory::processes::{self as procs_inv, Process};
use crate::inventory::services::{parse_rcctl, Service};
use crate::money::{self, MoneyCache, MoneyResult, MoneySlot};
use crate::buyvm::{self, BuyvmCache, BuyvmResult};
use crate::vultr::{self, ActionKind, ActionResult, VultrCache, VultrResult, VultrSlot};
use crate::ipc::protocol::{Event as IpcEvent, Request as IpcRequest};
use crate::ipc::server::Job;
use crate::ssh::collect::{
    spawn_processes_and_ports, spawn_rcctl_triple, CollectResult, InvResult, InvSlot, Slot,
};
use crate::ssh::{spawn_remote, RunEvent, RunHandle};
use crate::tmux::shell_quote;

pub const PAGE_STEP: usize = 10;

#[cfg(test)]
mod scroll_tests {
    use super::ScrollState;

    #[test]
    fn sticky_renders_bottom_regardless_of_offset() {
        let s = ScrollState::new_sticky();
        // total=100, viewport=20 → max=80; sticky should pin to 80.
        assert_eq!(s.render_start(100, 20), 80);
        assert_eq!(s.offset.get(), 80);
    }

    #[test]
    fn top_anchored_stays_at_zero_until_scrolled() {
        let s = ScrollState::new_top();
        assert_eq!(s.render_start(100, 20), 0);
        s.line_down();
        assert_eq!(s.render_start(100, 20), 1);
    }

    #[test]
    fn line_up_disables_sticky() {
        let s = ScrollState::new_sticky();
        s.render_start(100, 20); // offset becomes 80
        s.line_up();
        assert!(!s.sticky.get());
        assert_eq!(s.render_start(100, 20), 79);
    }

    #[test]
    fn down_scroll_clamps_and_re_enables_sticky() {
        let s = ScrollState::new_top();
        // total=10, viewport=20 → max=0; any down stays at 0, no sticky flip
        // (sticky on max==0 is the "no scroll needed" case, not "at bottom").
        s.line_down();
        assert_eq!(s.render_start(10, 20), 0);
        // total=30, viewport=10 → max=20
        for _ in 0..50 {
            s.line_down();
        }
        assert_eq!(s.render_start(30, 10), 20);
        assert!(s.sticky.get(), "reaching the bottom re-enables sticky");
    }

    #[test]
    fn page_up_and_page_down_jump_by_step() {
        let s = ScrollState::new_top();
        s.page_down();
        assert_eq!(s.offset.get(), 10);
        s.page_up();
        assert_eq!(s.offset.get(), 0);
    }

    #[test]
    fn to_top_and_to_bottom_set_endpoints() {
        let s = ScrollState::new_top();
        s.page_down();
        s.page_down();
        assert_eq!(s.offset.get(), 20);
        s.to_top();
        assert_eq!(s.offset.get(), 0);
        assert!(!s.sticky.get());
        s.to_bottom();
        assert!(s.sticky.get());
        assert_eq!(s.render_start(100, 20), 80);
    }

    #[test]
    fn render_clamps_offset_past_total() {
        let s = ScrollState::new_top();
        s.offset.set(9999);
        assert_eq!(s.render_start(50, 10), 40);
    }

    #[test]
    fn empty_content_yields_zero_start() {
        let s = ScrollState::new_top();
        assert_eq!(s.render_start(0, 20), 0);
    }

    #[test]
    fn line_down_starting_at_bottom_keeps_sticky() {
        let s = ScrollState::new_sticky();
        s.render_start(50, 10);
        // User mashes j past the bottom — should remain sticky, render unchanged.
        for _ in 0..10 {
            s.line_down();
        }
        assert!(s.sticky.get());
        assert_eq!(s.render_start(50, 10), 40);
    }
}

/// Per-pane scroll state. Used by every pane that renders a list / stream
/// longer than its viewport. Interior mutability via `Cell` so renderers
/// (which take `&App`) can clamp `offset` against the current total/
/// viewport and flip `sticky` when the user scrolls back to the bottom.
///
/// Conventions:
/// - `offset` is the row index from the top of the content.
/// - `sticky == true` ignores `offset` at render time and renders the
///   bottom of the content; useful for streaming panes that should
///   auto-follow new lines until the user scrolls up.
/// - Streaming panes (LogTail, AgentTail, Runner output) construct with
///   `ScrollState::new_sticky()`; tabular panes use `ScrollState::new_top()`.
#[derive(Debug, Default)]
pub struct ScrollState {
    pub offset: Cell<usize>,
    pub sticky: Cell<bool>,
}

impl Clone for ScrollState {
    fn clone(&self) -> Self {
        Self {
            offset: Cell::new(self.offset.get()),
            sticky: Cell::new(self.sticky.get()),
        }
    }
}

impl ScrollState {
    pub fn new_sticky() -> Self {
        Self {
            offset: Cell::new(0),
            sticky: Cell::new(true),
        }
    }

    pub fn new_top() -> Self {
        Self {
            offset: Cell::new(0),
            sticky: Cell::new(false),
        }
    }

    /// Renderer-side: compute the start row given current total content +
    /// viewport. Side effects: clamps `offset` to `[0, total-viewport]`;
    /// re-enables `sticky` when the clamped offset lands exactly at the
    /// bottom (so an explicit scroll-down-past-bottom resumes tailing).
    pub fn render_start(&self, total: usize, viewport: usize) -> usize {
        let max = total.saturating_sub(viewport);
        if self.sticky.get() {
            self.offset.set(max);
            return max;
        }
        let off = self.offset.get().min(max);
        if off == max && max > 0 {
            self.sticky.set(true);
        }
        self.offset.set(off);
        off
    }

    pub fn line_up(&self) {
        self.sticky.set(false);
        self.offset.set(self.offset.get().saturating_sub(1));
    }

    pub fn line_down(&self) {
        // Don't disable sticky on a down-scroll: if the user is already at
        // bottom (sticky == true), one more `j` should keep them there.
        // `render_start` will re-clamp.
        self.offset.set(self.offset.get().saturating_add(1));
    }

    pub fn page_up(&self) {
        self.sticky.set(false);
        self.offset.set(self.offset.get().saturating_sub(PAGE_STEP));
    }

    pub fn page_down(&self) {
        self.offset.set(self.offset.get().saturating_add(PAGE_STEP));
    }

    pub fn to_top(&self) {
        self.sticky.set(false);
        self.offset.set(0);
    }

    pub fn to_bottom(&self) {
        self.sticky.set(true);
    }

    /// Nudge `offset` so that row `selected` is inside the next viewport
    /// `[offset, offset+viewport)`. No-op when the row is already visible
    /// or when the viewport is empty. Disables `sticky` because the caller
    /// is driving by selection, not by tail position.
    pub fn ensure_visible(&self, selected: usize, total: usize, viewport: usize) {
        if total == 0 || viewport == 0 {
            return;
        }
        self.sticky.set(false);
        let max = total.saturating_sub(viewport);
        let mut off = self.offset.get().min(max);
        if selected < off {
            off = selected;
        } else if selected >= off + viewport {
            off = selected + 1 - viewport;
        }
        self.offset.set(off.min(max));
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
        AgentOutputLine::Out(s) => LineRecord { kind: LineKind::Out, line: s.clone() },
        AgentOutputLine::Err(s) => LineRecord { kind: LineKind::Err, line: s.clone() },
        AgentOutputLine::System(s) => LineRecord { kind: LineKind::System, line: s.clone() },
    }
}

fn output_line_to_record(l: &OutputLine) -> LineRecord {
    match l {
        OutputLine::Out(s) => LineRecord { kind: LineKind::Out, line: s.clone() },
        OutputLine::Err(s) => LineRecord { kind: LineKind::Err, line: s.clone() },
        OutputLine::Partial(s) => LineRecord { kind: LineKind::Out, line: s.clone() },
        OutputLine::System(s) => LineRecord { kind: LineKind::System, line: s.clone() },
    }
}

fn spawn_health_state(businesses: &[crate::config::Business]) -> HealthState {
    let business_names: Vec<String> =
        businesses.iter().map(|b| b.name.clone()).collect();
    let rows: Vec<Option<Health>> = vec![None; businesses.len()];
    let rx = health::spawn_health(businesses);
    HealthState {
        rx,
        rows,
        business_names,
        scroll: ScrollState::new_top(),
    }
}

/// Build a per-business `expected_ip` vector by joining each business's
/// `host` field to its `[[hosts]]` entry's `hostname`. The verdict logic
/// only acts on values that parse as `IpAddr`, so DNS-name hostnames are
/// passed through and naturally land at `Unknown`.
fn spawn_dns_state(config: &crate::config::Config) -> DnsState {
    let business_names: Vec<String> =
        config.businesses.iter().map(|b| b.name.clone()).collect();
    let expected_ips: Vec<Option<String>> = config
        .businesses
        .iter()
        .map(|b| {
            config
                .hosts
                .iter()
                .find(|h| h.name == b.host)
                .map(|h| h.display_hostname().to_string())
        })
        .collect();
    let rows = vec![None; config.businesses.len()];
    let rx = crate::inventory::dns::spawn_dns(&config.businesses, &expected_ips);
    DnsState {
        rx,
        rows,
        business_names,
        scroll: ScrollState::new_top(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Browse,
    Runner,
    Services,
    Shortcuts,
    AgentTail,
    Processes,
    Health,
    Vultr,
    Buyvm,
    Money,
    LogPicker,
    LogTail,
    History,
    Dns,
    Help,
}

pub struct BuyvmState {
    pub rx: Receiver<BuyvmResult>,
}

pub struct DnsState {
    pub rx: Receiver<crate::inventory::dns::DnsResult>,
    pub rows: Vec<Option<crate::inventory::dns::DnsCheck>>,
    pub business_names: Vec<String>,
    pub scroll: ScrollState,
}

impl DnsState {
    pub fn pending_count(&self) -> usize {
        self.rows.iter().filter(|r| r.is_none()).count()
    }
}

pub struct HistoryState {
    pub entries: Vec<crate::history::RunRecord>,
    pub selected: usize,
    pub scroll: ScrollState,
    pub error: Option<String>,
}

/// Hard cap on lines retained in `LogTailState.lines`. Tail can run for
/// hours on a busy host, so we drop the oldest line whenever the buffer
/// exceeds this. The Esc-to-kill flow tears down the whole state anyway,
/// so we don't need a precise time-based eviction.
const LOG_TAIL_BUFFER_MAX: usize = 5000;

pub struct LogTailState {
    pub alias: String,
    pub label: String,
    pub path: String,
    pub handle: Option<RunHandle>,
    /// Each entry is one terminal-display line. Stderr lines are kept too
    /// (tail -F emits "tail: file truncated" on rotation etc.) and rendered
    /// in a distinct color.
    pub lines: Vec<LogLine>,
    pub exit: Option<i32>,
    pub error: Option<String>,
    pub scroll: ScrollState,
}

#[derive(Debug, Clone)]
pub enum LogLine {
    Out(String),
    Err(String),
    System(String),
}

pub struct VultrState {
    pub rx: Receiver<VultrResult>,
    pub instances_raw: Option<Result<String, String>>,
    pub plans_raw: Option<Result<String, String>>,
}

#[derive(Debug, Clone)]
pub struct VultrConfirm {
    pub action: ActionKind,
    pub instance_id: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VultrToastKind {
    Firing,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub struct VultrToast {
    pub kind: VultrToastKind,
    pub message: String,
}

impl VultrState {
    fn slot_mut(&mut self, s: VultrSlot) -> &mut Option<Result<String, String>> {
        match s {
            VultrSlot::Instances => &mut self.instances_raw,
            VultrSlot::Plans => &mut self.plans_raw,
        }
    }

    fn all_filled(&self) -> bool {
        self.instances_raw.is_some() && self.plans_raw.is_some()
    }
}

pub struct MoneyState {
    pub rx: Receiver<MoneyResult>,
    pub stripe_raw: Option<Result<String, String>>,
    pub mercury_raw: Option<Result<String, String>>,
    /// Per-Connect-account raw responses, keyed by acct id. Sized
    /// against `expected_connect_ids` at construction.
    pub connect_raw: std::collections::HashMap<String, Result<String, String>>,
    /// Acct ids we expect responses for. `all_filled` cross-checks
    /// `connect_raw`'s length against this list.
    pub expected_connect_ids: Vec<String>,
}

impl MoneyState {
    fn record(&mut self, slot: MoneySlot, payload: Result<String, String>) {
        match slot {
            MoneySlot::Stripe => self.stripe_raw = Some(payload),
            MoneySlot::Mercury => self.mercury_raw = Some(payload),
            MoneySlot::StripeConnect(acct) => {
                self.connect_raw.insert(acct, payload);
            }
        }
    }

    fn all_filled(&self) -> bool {
        self.stripe_raw.is_some()
            && self.mercury_raw.is_some()
            && self.connect_raw.len() >= self.expected_connect_ids.len()
    }
}

pub struct HealthState {
    pub rx: Receiver<HealthResult>,
    /// Indexed in lock-step with the businesses slice the pane was opened
    /// against. `None` until that business's probe finishes.
    pub rows: Vec<Option<Health>>,
    /// Snapshot of the business list as of pane open — we render against
    /// this rather than `config.businesses` directly so a later config
    /// reload doesn't desync indices.
    pub business_names: Vec<String>,
    pub scroll: ScrollState,
}

impl HealthState {
    pub fn pending_count(&self) -> usize {
        self.rows.iter().filter(|r| r.is_none()).count()
    }
}

pub struct ProcessesState {
    pub host_alias: String,
    pub host_name: String,
    pub rx: Receiver<InvResult>,
    pub processes_raw: Option<Result<String, String>>,
    pub ports_raw: Option<Result<String, String>>,
    pub processes: Option<Vec<Process>>,
    pub ports: Option<Vec<ListeningSocket>>,
    pub error: Option<String>,
    pub scroll: ScrollState,
}

impl ProcessesState {
    fn slot_mut(&mut self, s: InvSlot) -> &mut Option<Result<String, String>> {
        match s {
            InvSlot::Processes => &mut self.processes_raw,
            InvSlot::Ports => &mut self.ports_raw,
        }
    }

    fn all_slots_filled(&self) -> bool {
        self.processes_raw.is_some() && self.ports_raw.is_some()
    }

    fn try_compute(&mut self) {
        if !self.all_slots_filled() || self.error.is_some() {
            return;
        }
        if self.processes.is_some() && self.ports.is_some() {
            return;
        }
        let p_raw = self.processes_raw.as_ref().unwrap();
        let s_raw = self.ports_raw.as_ref().unwrap();
        for r in [p_raw, s_raw] {
            if let Err(e) = r {
                self.error = Some(e.clone());
                return;
            }
        }
        let procs = procs_inv::parse(p_raw.as_ref().unwrap());
        self.processes = Some(procs_inv::top_by_cpu(&procs, 20));
        self.ports = Some(ports_inv::parse(s_raw.as_ref().unwrap()));
    }
}

/// One completed (or in-flight) agent-issued command, accumulated for
/// the tail view.
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
    /// True for entries reconstructed from the history DB on startup.
    /// Lets the UI tag them visually and skip re-persisting on a
    /// hypothetical "replay" (not implemented).
    pub from_history: bool,
}

#[derive(Debug, Clone)]
pub enum AgentOutputLine {
    Out(String),
    Err(String),
    System(String),
}

/// In-flight agent execution. While `Some(_)`, the main loop forwards each
/// RunEvent to (a) `history.last_mut()` (the corresponding entry) and (b)
/// the socket worker via `response_tx`.
pub struct AgentExec {
    pub handle: RunHandle,
    pub response_tx: std::sync::mpsc::Sender<IpcEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFocus {
    /// Typing a command to run.
    Command,
    /// Typing a password into the modal.
    Password,
}

#[derive(Debug, Clone)]
pub enum OutputLine {
    Out(String),
    Err(String),
    Partial(String),
    System(String),
}

#[derive(Debug, Default)]
pub struct RunnerState {
    pub input: String,
    pub output: Vec<OutputLine>,
    pub focus: Option<InputFocus>,
    pub password: String,
    pub exit_code: Option<i32>,
    pub running: bool,
    /// Set when `submit_command` (or a shortcut fire) starts a run. Used
    /// to construct the history record on completion. None when no run
    /// is currently in flight or has been observed since the last clear.
    pub current_alias: Option<String>,
    pub current_cmd: Option<String>,
    pub current_started_at: Option<Instant>,
    pub current_started_at_unix: Option<i64>,
    pub scroll: ScrollState,
}

pub struct ServicesState {
    pub host_alias: String,
    pub host_name: String,
    pub rx: Receiver<CollectResult>,
    pub on: Option<Result<String, String>>,
    pub started: Option<Result<String, String>>,
    pub failed: Option<Result<String, String>>,
    pub services: Option<Vec<Service>>,
    pub error: Option<String>,
    pub scroll: ScrollState,
}

impl ServicesState {
    fn slot_mut(&mut self, s: Slot) -> &mut Option<Result<String, String>> {
        match s {
            Slot::On => &mut self.on,
            Slot::Started => &mut self.started,
            Slot::Failed => &mut self.failed,
        }
    }

    fn all_slots_filled(&self) -> bool {
        self.on.is_some() && self.started.is_some() && self.failed.is_some()
    }

    fn try_compute(&mut self) {
        if !self.all_slots_filled() || self.services.is_some() || self.error.is_some() {
            return;
        }
        let (on, started, failed) = (
            self.on.as_ref().unwrap(),
            self.started.as_ref().unwrap(),
            self.failed.as_ref().unwrap(),
        );
        for r in [on, started, failed] {
            if let Err(e) = r {
                self.error = Some(e.clone());
                return;
            }
        }
        self.services = Some(parse_rcctl(
            on.as_ref().unwrap(),
            started.as_ref().unwrap(),
            failed.as_ref().unwrap(),
        ));
    }
}

pub struct App {
    pub config: Config,
    pub selected: usize,
    pub status: String,
    pub should_quit: bool,
    pub launch_ssh: Option<String>,
    pub mode: Mode,
    pub help_origin: Option<Mode>,
    /// Money pane filter. `None` = all rows; `Some(idx)` = show only
    /// rows belonging to `money_filtered_businesses()[idx]`. Cycled via
    /// `f` in the money pane.
    pub money_filter: Option<usize>,
    /// Inline toast for the Vultr pane (action firing / success / error).
    /// Rendered at the bottom of the `v` pane so the user keeps the
    /// instance table in view while the action settles. Cleared on
    /// close_vultr or replaced by the next action.
    pub vultr_toast: Option<VultrToast>,
    pub runner: RunnerState,
    pub run_handle: Option<RunHandle>,
    pub services: Option<ServicesState>,
    pub processes_pane: Option<ProcessesState>,
    pub health_pane: Option<HealthState>,
    pub vultr_pane: Option<VultrState>,
    pub vultr_cache: Option<VultrCache>,
    pub vultr_error: Option<String>,
    /// True once `start_vultr_fetch` has run at least once — distinguishes
    /// "API key not set" from "fetch in flight" for the empty pane.
    pub vultr_fetch_attempted: bool,
    /// Index into `vultr_cache.instances` for the currently highlighted
    /// row. Always clamped to a valid value when rendering, so a fetch
    /// that returns fewer instances than before doesn't crash the pane.
    pub vultr_selected: usize,
    /// In-flight action confirmation prompt (modal overlay on the vultr
    /// pane). None when no confirm is pending.
    pub vultr_confirm: Option<VultrConfirm>,
    /// Receiver for the most recently fired action (one-shot — dropped
    /// after the result is ingested).
    pub vultr_action_rx: Option<Receiver<ActionResult>>,

    pub buyvm_pane: Option<BuyvmState>,
    pub buyvm_cache: Option<BuyvmCache>,
    pub buyvm_error: Option<String>,
    pub buyvm_fetch_attempted: bool,
    pub buyvm_scroll: ScrollState,

    pub money_pane: Option<MoneyState>,
    pub money_cache: Option<MoneyCache>,
    /// True once `start_money_fetch` has run at least once. Mirrors the
    /// Vultr flag so the empty-state UI can distinguish "press r to fetch"
    /// from "fetching…".
    pub money_fetch_attempted: bool,

    pub log_tail: Option<LogTailState>,

    pub history_pane: Option<HistoryState>,

    pub dns_pane: Option<DnsState>,

    /// Postmark stats keyed by business name. Inserted as each thread's
    /// result arrives so the Browse detail can render partial data.
    pub postmark_results:
        std::collections::HashMap<String, Result<crate::postmark::PostmarkStats, String>>,
    /// Live receiver while a fetch is in flight. None when no fetch has
    /// started, or after the last result has been drained.
    pub postmark_rx: Option<Receiver<crate::postmark::PostmarkResult>>,
    pub postmark_fetch_attempted: bool,

    // agent-as-operator state
    pub jobs_rx: Option<Receiver<Job>>,
    pub agent_history: Vec<AgentHistoryEntry>,
    pub agent_active: Option<AgentExec>,
    pub agent_queue: VecDeque<Job>,

    /// SQLite-backed run history. `None` when the DB couldn't be opened —
    /// helm continues to work but state is ephemeral. Mutex via &mut self
    /// is sufficient because every call is on the main loop thread.
    pub history: Option<HistoryStore>,

    /// Scroll state for the AgentTail pane. Mode-level rather than
    /// per-entry because the pane shows one continuous transcript.
    pub agent_tail_scroll: ScrollState,
    /// Scroll state for the Vultr table (no in-pane state struct exists —
    /// the cache lives directly on App, so the scroll lives here too).
    pub vultr_scroll: ScrollState,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("selected", &self.selected)
            .field("mode", &self.mode)
            .field("running", &self.runner.running)
            .finish()
    }
}

impl App {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            selected: 0,
            status: String::new(),
            should_quit: false,
            launch_ssh: None,
            mode: Mode::Browse,
            help_origin: None,
            money_filter: None,
            vultr_toast: None,
            runner: RunnerState::default(),
            run_handle: None,
            services: None,
            processes_pane: None,
            health_pane: None,
            vultr_pane: None,
            vultr_cache: None,
            vultr_error: None,
            vultr_fetch_attempted: false,
            vultr_selected: 0,
            vultr_confirm: None,
            vultr_action_rx: None,
            buyvm_pane: None,
            buyvm_cache: None,
            buyvm_error: None,
            buyvm_fetch_attempted: false,
            buyvm_scroll: ScrollState::new_top(),
            money_pane: None,
            money_cache: None,
            money_fetch_attempted: false,
            log_tail: None,
            history_pane: None,
            dns_pane: None,
            postmark_results: std::collections::HashMap::new(),
            postmark_rx: None,
            postmark_fetch_attempted: false,
            jobs_rx: None,
            agent_history: Vec::new(),
            agent_active: None,
            agent_queue: VecDeque::new(),
            history: None,
            agent_tail_scroll: ScrollState::new_sticky(),
            vultr_scroll: ScrollState::new_top(),
        }
    }

    /// Attach a SQLite-backed history store and rehydrate `agent_history`
    /// with the most-recent N agent runs in chronological order. Older
    /// entries are at index 0 so the AgentTail's append-only render keeps
    /// new entries at the bottom.
    pub fn attach_history(&mut self, store: HistoryStore, load_limit: usize) {
        // Cap retained rows so the DB doesn't grow unbounded across sessions.
        // 5000 is a generous ceiling for solo-operator workloads — covers
        // weeks of `helm exec` activity even on busy days.
        if let Err(e) = store.prune_to(5000) {
            tracing::warn!("history: prune_to(5000) failed: {e}");
        }
        match store.recent_runs(Some(RunSource::Agent), load_limit) {
            Ok(mut runs) => {
                // recent_runs returns newest-first; reverse so chronological.
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
    /// one if nothing is running.
    pub fn ingest_jobs(&mut self) {
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
        let IpcRequest::Exec { alias, cmd } = job.request;
        let resolved_alias = if alias.is_empty() {
            self.selected_host().map(|h| h.ssh_alias.clone())
        } else {
            Some(alias)
        };
        let Some(alias) = resolved_alias else {
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
            output: vec![AgentOutputLine::System(format!(
                "$ ssh {alias} '{cmd}'"
            ))],
            exit: None,
            from_history: false,
        };
        match spawn_remote(&alias, &cmd) {
            Ok(handle) => {
                self.agent_history.push(entry);
                self.agent_active = Some(AgentExec {
                    handle,
                    response_tx: job.response_tx,
                });
            }
            Err(e) => {
                let msg = format!("spawn failed: {e}");
                entry
                    .output
                    .push(AgentOutputLine::System(msg.clone()));
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
            // Stream went away without a Done event — finalize defensively.
            if let Some(active) = self.agent_active.take() {
                if let Some(entry) = self.agent_history.last_mut() {
                    if entry.exit.is_none() {
                        entry.exit = Some(-1);
                        entry
                            .output
                            .push(AgentOutputLine::System("(channel closed)".into()));
                    }
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
                // Partial lines (e.g. password prompts) — surface as system
                // line in history but do not forward to client. A agent
                // command requiring a password modal is a workflow problem;
                // the user can intervene from the TUI.
                entry.output.push(AgentOutputLine::System(
                    "(password prompt detected — agent command paused; \
                     answer in TUI Runner if intended)".into(),
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
                entry.output.push(AgentOutputLine::System(format!("error: {msg}")));
                self.agent_active = None;
                self.persist_last_agent_entry();
                self.maybe_start_next_agent_job();
            }
        }
    }

    /// Persist the most recent agent_history entry to SQLite. Called from
    /// the two terminal `apply_agent_event` paths (Done / Error). Silently
    /// no-ops when the DB isn't attached or when the entry was loaded
    /// from history rather than live.
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
        let lines: Vec<LineRecord> = entry
            .output
            .iter()
            .map(agent_line_to_record)
            .collect();
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

    pub fn open_agent_tail(&mut self) {
        self.mode = Mode::AgentTail;
    }

    pub fn close_agent_tail(&mut self) {
        if self.mode == Mode::AgentTail {
            self.mode = Mode::Browse;
        }
    }

    /// Header chip text describing current agent activity, regardless of
    /// the user's currently-focused mode.
    pub fn agent_indicator(&self) -> String {
        if let Some(active) = self.agent_active.as_ref() {
            let snippet = truncate(&active.handle.command, 40);
            format!("🤖 {} › {}", active.handle.alias, snippet)
        } else {
            format!("🤖 idle ({})", self.agent_history.len())
        }
    }

    pub fn hosts(&self) -> &[Host] {
        &self.config.hosts
    }

    pub fn selected_host(&self) -> Option<&Host> {
        self.hosts().get(self.selected)
    }

    pub fn next_host(&mut self) {
        if self.hosts().is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.hosts().len();
    }

    pub fn prev_host(&mut self) {
        if self.hosts().is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = self.hosts().len() - 1;
        } else {
            self.selected -= 1;
        }
    }

    pub fn request_ssh(&mut self) {
        if let Some(h) = self.selected_host() {
            self.launch_ssh = Some(h.ssh_alias.clone());
        }
    }

    pub fn open_runner(&mut self) {
        if self.selected_host().is_none() {
            self.status = "no host selected".into();
            return;
        }
        self.mode = Mode::Runner;
        self.runner = RunnerState {
            focus: Some(InputFocus::Command),
            scroll: ScrollState::new_sticky(),
            ..RunnerState::default()
        };
    }

    pub fn close_runner(&mut self) {
        // Drop any in-flight handle; child gets SIGPIPE on next write.
        self.run_handle = None;
        self.mode = Mode::Browse;
    }

    pub fn open_services(&mut self) {
        let Some(host) = self.selected_host() else {
            self.status = "no host selected".into();
            return;
        };
        let alias = host.ssh_alias.clone();
        let name = host.name.clone();
        let rx = spawn_rcctl_triple(&alias);
        self.mode = Mode::Services;
        self.services = Some(ServicesState {
            host_alias: alias,
            host_name: name,
            rx,
            on: None,
            started: None,
            failed: None,
            services: None,
            error: None,
            scroll: ScrollState::new_top(),
        });
    }

    pub fn refresh_services(&mut self) {
        let Some(s) = self.services.as_ref() else {
            return;
        };
        let alias = s.host_alias.clone();
        let name = s.host_name.clone();
        let rx = spawn_rcctl_triple(&alias);
        self.services = Some(ServicesState {
            host_alias: alias,
            host_name: name,
            rx,
            on: None,
            started: None,
            failed: None,
            services: None,
            error: None,
            scroll: ScrollState::new_top(),
        });
    }

    pub fn close_services(&mut self) {
        self.services = None;
        self.mode = Mode::Browse;
    }

    pub fn open_processes(&mut self) {
        let Some(host) = self.selected_host() else {
            self.status = "no host selected".into();
            return;
        };
        let alias = host.ssh_alias.clone();
        let name = host.name.clone();
        let rx = spawn_processes_and_ports(&alias);
        self.mode = Mode::Processes;
        self.processes_pane = Some(ProcessesState {
            host_alias: alias,
            host_name: name,
            rx,
            processes_raw: None,
            ports_raw: None,
            processes: None,
            ports: None,
            error: None,
            scroll: ScrollState::new_top(),
        });
    }

    pub fn refresh_processes(&mut self) {
        let Some(s) = self.processes_pane.as_ref() else {
            return;
        };
        let alias = s.host_alias.clone();
        let name = s.host_name.clone();
        let rx = spawn_processes_and_ports(&alias);
        self.processes_pane = Some(ProcessesState {
            host_alias: alias,
            host_name: name,
            rx,
            processes_raw: None,
            ports_raw: None,
            processes: None,
            ports: None,
            error: None,
            scroll: ScrollState::new_top(),
        });
    }

    pub fn close_processes(&mut self) {
        self.processes_pane = None;
        self.mode = Mode::Browse;
    }

    pub fn open_health(&mut self) {
        if self.config.businesses.is_empty() {
            self.status = "no businesses configured".into();
            return;
        }
        self.mode = Mode::Health;
        self.health_pane = Some(spawn_health_state(&self.config.businesses));
    }

    pub fn refresh_health(&mut self) {
        if self.health_pane.is_some() {
            self.health_pane = Some(spawn_health_state(&self.config.businesses));
        }
    }

    pub fn close_health(&mut self) {
        self.health_pane = None;
        self.mode = Mode::Browse;
    }

    pub fn ingest_health_events(&mut self) {
        let Some(s) = self.health_pane.as_mut() else {
            return;
        };
        while let Ok(res) = s.rx.try_recv() {
            if let Some(slot) = s.rows.get_mut(res.idx) {
                *slot = Some(res.health);
            }
        }
    }

    /// Fire a background Vultr fetch using `VULTR_API_KEY` from the
    /// environment. No-op if the env var is unset. Replaces any existing
    /// in-flight pane state (refresh is idempotent).
    pub fn start_vultr_fetch(&mut self) {
        let Ok(key) = std::env::var("VULTR_API_KEY") else {
            return;
        };
        if key.trim().is_empty() {
            return;
        }
        self.vultr_fetch_attempted = true;
        self.vultr_error = None;
        let rx = vultr::spawn_vultr_fetch(key);
        self.vultr_pane = Some(VultrState {
            rx,
            instances_raw: None,
            plans_raw: None,
        });
    }

    pub fn ingest_vultr_events(&mut self) {
        // Phase 1: drain into the in-flight state.
        let ready = {
            let Some(s) = self.vultr_pane.as_mut() else {
                return;
            };
            while let Ok(res) = s.rx.try_recv() {
                *s.slot_mut(res.slot) = Some(res.output);
            }
            s.all_filled()
        };
        if !ready {
            return;
        }
        // Phase 2: both slots filled — promote out of the pane so we can
        // mutate `self` freely without holding a borrow.
        let state = self.vultr_pane.take().unwrap();
        let i_raw = state.instances_raw.unwrap();
        let p_raw = state.plans_raw.unwrap();
        let i_body = match i_raw {
            Ok(b) => b,
            Err(e) => {
                self.vultr_error = Some(e);
                return;
            }
        };
        let p_body = match p_raw {
            Ok(b) => b,
            Err(e) => {
                self.vultr_error = Some(e);
                return;
            }
        };
        let instances = match vultr::parse_instances(&i_body) {
            Ok(v) => v,
            Err(e) => {
                self.vultr_error = Some(e);
                return;
            }
        };
        let plans = match vultr::parse_plans(&p_body) {
            Ok(v) => v,
            Err(e) => {
                self.vultr_error = Some(e);
                return;
            }
        };
        self.vultr_cache = Some(VultrCache { instances, plans });
    }

    pub fn open_vultr(&mut self) {
        self.mode = Mode::Vultr;
        // Clamp selection to current instance count so a fetch that
        // shrank the list (e.g. after a deletion) doesn't keep us on a
        // non-existent row.
        let len = self
            .vultr_cache
            .as_ref()
            .map(|c| c.instances.len())
            .unwrap_or(0);
        if len > 0 && self.vultr_selected >= len {
            self.vultr_selected = len - 1;
        }
    }

    pub fn vultr_select_next(&mut self) {
        let Some(cache) = self.vultr_cache.as_ref() else {
            return;
        };
        if cache.instances.is_empty() {
            return;
        }
        self.vultr_selected = (self.vultr_selected + 1).min(cache.instances.len() - 1);
    }

    pub fn vultr_select_prev(&mut self) {
        self.vultr_selected = self.vultr_selected.saturating_sub(1);
    }

    /// Stage a confirm modal for `action` against the currently selected
    /// instance. No-op when the pane has no cache or the cursor sits on
    /// nothing. The action itself does not fire until `vultr_confirm_action`
    /// is called.
    ///
    /// Refuses to stage a second action while one is in flight: the
    /// previous request's result would otherwise be dropped on the floor
    /// (single-shot receiver), masking failures and skipping the
    /// post-success inventory refresh.
    pub fn vultr_request_action(&mut self, action: ActionKind) {
        if self.vultr_action_rx.is_some() {
            self.vultr_toast = Some(VultrToast {
                kind: VultrToastKind::Error,
                message: "previous action still in flight — wait for it".into(),
            });
            return;
        }
        let Some(cache) = self.vultr_cache.as_ref() else {
            return;
        };
        let Some(inst) = cache.instances.get(self.vultr_selected) else {
            return;
        };
        self.vultr_confirm = Some(VultrConfirm {
            action,
            instance_id: inst.id.clone(),
            label: if inst.label.is_empty() {
                inst.id.clone()
            } else {
                inst.label.clone()
            },
        });
    }

    pub fn vultr_cancel_action(&mut self) {
        self.vultr_confirm = None;
    }

    /// Confirm the pending action: fires a `POST` against the Vultr API
    /// in a background thread. Status line shows immediate "firing…"
    /// feedback; the result lands later via `ingest_vultr_action_events`.
    pub fn vultr_confirm_action(&mut self) {
        let Some(confirm) = self.vultr_confirm.take() else {
            return;
        };
        let Ok(key) = std::env::var("VULTR_API_KEY") else {
            self.vultr_toast = Some(VultrToast {
                kind: VultrToastKind::Error,
                message: "VULTR_API_KEY not set — action skipped".into(),
            });
            return;
        };
        if key.trim().is_empty() {
            self.vultr_toast = Some(VultrToast {
                kind: VultrToastKind::Error,
                message: "VULTR_API_KEY empty — action skipped".into(),
            });
            return;
        }
        self.vultr_toast = Some(VultrToast {
            kind: VultrToastKind::Firing,
            message: format!(
                "{}: firing on {}…",
                confirm.action.label(),
                confirm.label
            ),
        });
        let rx = vultr::spawn_vultr_action(
            key,
            confirm.action,
            confirm.instance_id,
            confirm.label,
        );
        self.vultr_action_rx = Some(rx);
    }

    /// Drain the one-shot action result channel. On success, refire the
    /// instance fetch so the table picks up new power_status / a new
    /// snapshot row. On failure, surface the error in the status line.
    pub fn ingest_vultr_action_events(&mut self) {
        let Some(rx) = self.vultr_action_rx.as_ref() else {
            return;
        };
        let Ok(res) = rx.try_recv() else {
            return;
        };
        self.vultr_action_rx = None;
        match res.outcome {
            Ok(_) => {
                self.vultr_toast = Some(VultrToast {
                    kind: VultrToastKind::Success,
                    message: format!(
                        "{}: {} ✓ (refreshing inventory)",
                        res.action.label(),
                        res.label
                    ),
                });
                // Vultr's state transition takes a beat; the next fetch
                // will reflect it. Fire-and-forget.
                self.start_vultr_fetch();
            }
            Err(e) => {
                self.vultr_toast = Some(VultrToast {
                    kind: VultrToastKind::Error,
                    message: format!("{} on {} failed: {e}", res.action.label(), res.label),
                });
            }
        }
    }

    /// Fire a background BuyVM fetch using `BUYVM_API_KEY` (and optional
    /// `BUYVM_API_BASE`) from the environment. No-op if the key is unset.
    pub fn start_buyvm_fetch(&mut self) {
        let Ok(key) = std::env::var("BUYVM_API_KEY") else {
            return;
        };
        if key.trim().is_empty() {
            return;
        }
        let base = std::env::var("BUYVM_API_BASE")
            .unwrap_or_else(|_| buyvm::DEFAULT_API_BASE.to_string());
        self.buyvm_fetch_attempted = true;
        self.buyvm_error = None;
        let rx = buyvm::spawn_buyvm_fetch(key, base.clone());
        self.buyvm_pane = Some(BuyvmState { rx });
        // Keep the base around for the cache so the UI can show it on
        // refresh errors / future per-service deep links.
        if let Some(cache) = self.buyvm_cache.as_mut() {
            cache.api_base = base;
        }
    }

    pub fn ingest_buyvm_events(&mut self) {
        let Some(s) = self.buyvm_pane.as_mut() else {
            return;
        };
        let Ok(res) = s.rx.try_recv() else {
            return;
        };
        // Single-shot fetch — drop the pane regardless of outcome.
        self.buyvm_pane = None;
        let body = match res.output {
            Ok(b) => b,
            Err(e) => {
                self.buyvm_error = Some(e);
                return;
            }
        };
        match buyvm::parse_services(&body) {
            Ok(services) => {
                let api_base = std::env::var("BUYVM_API_BASE")
                    .unwrap_or_else(|_| buyvm::DEFAULT_API_BASE.to_string());
                self.buyvm_cache = Some(BuyvmCache { services, api_base });
            }
            Err(e) => self.buyvm_error = Some(e),
        }
    }

    pub fn open_buyvm(&mut self) {
        self.mode = Mode::Buyvm;
    }

    pub fn refresh_buyvm(&mut self) {
        self.start_buyvm_fetch();
    }

    pub fn close_buyvm(&mut self) {
        if self.mode == Mode::Buyvm {
            self.mode = Mode::Browse;
        }
    }

    pub fn refresh_vultr(&mut self) {
        self.start_vultr_fetch();
    }

    pub fn close_vultr(&mut self) {
        if self.mode == Mode::Vultr {
            self.mode = Mode::Browse;
            self.vultr_toast = None;
        }
    }

    /// Fire a background Stripe + Mercury fetch. No env-var check — both
    /// CLIs surface their own auth errors which then render in the pane.
    /// Replaces any in-flight state (refresh is idempotent).
    pub fn start_money_fetch(&mut self) {
        self.money_fetch_attempted = true;
        let connect_ids = self.connect_account_ids();
        let rx = money::spawn_money_fetch(&connect_ids);
        self.money_pane = Some(MoneyState {
            rx,
            stripe_raw: None,
            mercury_raw: None,
            connect_raw: std::collections::HashMap::new(),
            expected_connect_ids: connect_ids,
        });
    }

    /// Distinct, non-empty Stripe Connect account ids declared by any
    /// business. Preserves first-seen order so threads spawn deterministically.
    fn connect_account_ids(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for b in &self.config.businesses {
            if let Some(id) = b.stripe_account_id.as_ref() {
                let t = id.trim();
                if !t.is_empty() && seen.insert(t.to_string()) {
                    out.push(t.to_string());
                }
            }
        }
        out
    }

    pub fn ingest_money_events(&mut self) {
        let ready = {
            let Some(s) = self.money_pane.as_mut() else {
                return;
            };
            while let Ok(res) = s.rx.try_recv() {
                s.record(res.slot, res.output);
            }
            s.all_filled()
        };
        if !ready {
            return;
        }
        let state = self.money_pane.take().unwrap();
        let stripe_raw = state.stripe_raw.unwrap();
        let mercury_raw = state.mercury_raw.unwrap();
        let connect_raw = state.connect_raw;

        let mut cache = MoneyCache::default();
        match stripe_raw {
            Ok(body) => match money::parse_stripe_balance(&body) {
                Ok(s) => cache.stripe = Some(s),
                Err(e) => cache.stripe_error = Some(e),
            },
            Err(e) => cache.stripe_error = Some(e),
        }
        for (acct_id, raw) in connect_raw {
            match raw {
                Ok(body) => match money::parse_stripe_balance(&body) {
                    Ok(s) => {
                        cache.stripe_connect.insert(acct_id, s);
                    }
                    Err(e) => {
                        cache.stripe_connect_errors.insert(acct_id, e);
                    }
                },
                Err(e) => {
                    cache.stripe_connect_errors.insert(acct_id, e);
                }
            }
        }
        match mercury_raw {
            Ok(body) => match money::parse_mercury_accounts(&body) {
                Ok(v) => cache.mercury = v,
                Err(e) => cache.mercury_error = Some(e),
            },
            Err(e) => cache.mercury_error = Some(e),
        }
        self.money_cache = Some(cache);
    }

    /// Logs applicable to the currently selected host: built-in OpenBSD
    /// defaults + any `[[logs]]` from config that match. Config entries
    /// take precedence on key collision (so a `[[logs]] key = "m"` for a
    /// specific host overrides the default `messages`).
    pub fn applicable_logs(&self) -> Vec<Log> {
        let Some(host) = self.selected_host() else {
            return Vec::new();
        };
        let alias = host.ssh_alias.clone();
        let mut out: Vec<Log> = self
            .config
            .logs
            .iter()
            .filter(|l| l.applies_to(&alias))
            .cloned()
            .collect();
        for d in builtin_logs() {
            if !out.iter().any(|l| l.key == d.key) {
                out.push(d);
            }
        }
        out
    }

    pub fn open_log_picker(&mut self) {
        if self.selected_host().is_none() {
            self.status = "no host selected".into();
            return;
        }
        if self.applicable_logs().is_empty() {
            self.status = "no logs for this host".into();
            return;
        }
        self.mode = Mode::LogPicker;
    }

    pub fn close_log_picker(&mut self) {
        if self.mode == Mode::LogPicker {
            self.mode = Mode::Browse;
        }
    }

    /// Try to launch the log tail bound to `key`. Returns true if a log
    /// matched and a tail was spawned. On spawn failure, surfaces the
    /// error in the log_tail state so the UI shows it.
    pub fn fire_log(&mut self, key: char) -> bool {
        let Some(host) = self.selected_host() else {
            return false;
        };
        let alias = host.ssh_alias.clone();
        let log = self.applicable_logs().into_iter().find(|l| l.key == key);
        let Some(log) = log else { return false };

        // OpenBSD `tail(1)` supports `-f` only (GNU's `-F` "follow by name
        // across rotations" is not portable). The remote `newsyslog` runs
        // infrequently enough that operators reopen the pane after a
        // rotation rather than depending on follow-by-name.
        let cmd = format!("tail -n 200 -f {}", shell_quote(&log.path));
        self.mode = Mode::LogTail;
        let mut state = LogTailState {
            alias: alias.clone(),
            label: log.label.clone(),
            path: log.path.clone(),
            handle: None,
            lines: vec![LogLine::System(format!(
                "$ ssh {alias} 'tail -n 200 -f {}'",
                log.path
            ))],
            exit: None,
            error: None,
            scroll: ScrollState::new_sticky(),
        };
        match crate::ssh::spawn_remote(&alias, &cmd) {
            Ok(handle) => state.handle = Some(handle),
            Err(e) => {
                state.error = Some(format!("spawn failed: {e}"));
            }
        }
        self.log_tail = Some(state);
        true
    }

    pub fn close_log_tail(&mut self) {
        // Drop the handle — child gets SIGPIPE on next stdout write. The
        // remote `tail -F` may linger for up to ~60s of buffered output
        // before its sshd reaper closes the channel. Acceptable for v1.
        self.log_tail = None;
        if self.mode == Mode::LogTail {
            self.mode = Mode::Browse;
        }
    }

    pub fn ingest_log_tail_events(&mut self) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(state) = self.log_tail.as_ref() {
            if let Some(handle) = state.handle.as_ref() {
                loop {
                    match handle.rx.try_recv() {
                        Ok(ev) => events.push(ev),
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
            }
        }
        let Some(state) = self.log_tail.as_mut() else {
            return;
        };
        for ev in events {
            match ev {
                RunEvent::Out(l) => state.lines.push(LogLine::Out(l)),
                RunEvent::Err(l) => state.lines.push(LogLine::Err(l)),
                RunEvent::Partial(_) | RunEvent::NeedPassword => {
                    // Logs we tail should be world-readable; if a password
                    // prompt appears the operator picked the wrong path.
                    // Surface it as system text rather than opening a modal.
                    state.lines.push(LogLine::System(
                        "(password prompt — pick a world-readable log path)".into(),
                    ));
                }
                RunEvent::Done(code) => {
                    state.exit = Some(code);
                    state
                        .lines
                        .push(LogLine::System(format!("tail exited {code}")));
                }
                RunEvent::Error(msg) => {
                    state.error = Some(msg.clone());
                    state.lines.push(LogLine::System(format!("error: {msg}")));
                }
            }
            // Cap retained buffer.
            if state.lines.len() > LOG_TAIL_BUFFER_MAX {
                let drop = state.lines.len() - LOG_TAIL_BUFFER_MAX;
                state.lines.drain(0..drop);
            }
        }
        if disconnected && state.exit.is_none() {
            state.exit = Some(-1);
            state
                .lines
                .push(LogLine::System("(channel closed)".into()));
        }
    }

    pub fn open_money(&mut self) {
        self.mode = Mode::Money;
        // Lazy first fetch — don't hit the network at startup the way
        // Vultr does, since Stripe / Mercury are heavier and the operator
        // may never open the pane.
        if !self.money_fetch_attempted {
            self.start_money_fetch();
        }
    }

    pub fn refresh_money(&mut self) {
        self.money_cache = None;
        self.start_money_fetch();
    }

    pub fn close_money(&mut self) {
        if self.mode == Mode::Money {
            self.mode = Mode::Browse;
        }
    }

    /// Businesses with at least one money linkage (Mercury or Stripe
    /// Connect). Cycled via `f` in the money pane.
    pub fn money_filtered_businesses(&self) -> Vec<&crate::config::Business> {
        self.config
            .businesses
            .iter()
            .filter(|b| {
                b.mercury_account_id
                    .as_deref()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
                    || b.stripe_account_id
                        .as_deref()
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false)
            })
            .collect()
    }

    /// Advance money filter: None → 0 → 1 → … → N-1 → None. No-op when
    /// there are no eligible businesses.
    pub fn cycle_money_filter(&mut self) {
        let count = self.money_filtered_businesses().len();
        if count == 0 {
            self.status = "no businesses have a stripe/mercury linkage".into();
            return;
        }
        self.money_filter = match self.money_filter {
            None => Some(0),
            Some(i) if i + 1 < count => Some(i + 1),
            Some(_) => None,
        };
    }

    /// The business the money filter is currently pinned to, if any.
    pub fn money_filter_business(&self) -> Option<&crate::config::Business> {
        let idx = self.money_filter?;
        self.money_filtered_businesses().into_iter().nth(idx)
    }

    /// Open the history pane. Loads up to 200 most-recent runs (both
    /// sources) from the SQLite store. If history is unattached the pane
    /// still opens but renders an empty-state error.
    pub fn open_history(&mut self) {
        self.mode = Mode::History;
        let state = match self.history.as_ref() {
            Some(store) => match store.recent_runs(None, 200) {
                Ok(entries) => HistoryState {
                    entries,
                    selected: 0,
                    scroll: ScrollState::new_top(),
                    error: None,
                },
                Err(e) => HistoryState {
                    entries: Vec::new(),
                    selected: 0,
                    scroll: ScrollState::new_top(),
                    error: Some(format!("history load failed: {e}")),
                },
            },
            None => HistoryState {
                entries: Vec::new(),
                selected: 0,
                scroll: ScrollState::new_top(),
                error: Some("history db unavailable".into()),
            },
        };
        self.history_pane = Some(state);
    }

    pub fn refresh_history(&mut self) {
        if self.mode == Mode::History {
            self.open_history();
        }
    }

    pub fn close_history(&mut self) {
        if self.mode == Mode::History {
            self.history_pane = None;
            self.mode = Mode::Browse;
        }
    }

    pub fn history_next(&mut self) {
        if let Some(s) = self.history_pane.as_mut() {
            if !s.entries.is_empty() {
                s.selected = (s.selected + 1).min(s.entries.len() - 1);
            }
        }
    }

    pub fn history_prev(&mut self) {
        if let Some(s) = self.history_pane.as_mut() {
            s.selected = s.selected.saturating_sub(1);
        }
    }

    /// Fire one Postmark fetch per business that supplies a server token.
    /// Idempotent: re-firing replaces any in-flight receiver but keeps
    /// already-resolved results in the cache so the UI doesn't blink.
    pub fn start_postmark_fetch(&mut self) {
        self.postmark_fetch_attempted = true;
        let rx = crate::postmark::spawn_postmark_fetch(&self.config.businesses);
        self.postmark_rx = Some(rx);
    }

    pub fn ingest_postmark_events(&mut self) {
        let Some(rx) = self.postmark_rx.as_ref() else {
            return;
        };
        let mut all_done = false;
        loop {
            match rx.try_recv() {
                Ok(res) => {
                    self.postmark_results.insert(res.business_name, res.outcome);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    all_done = true;
                    break;
                }
            }
        }
        if all_done {
            self.postmark_rx = None;
        }
    }

    pub fn open_dns(&mut self) {
        if self.config.businesses.is_empty() {
            self.status = "no businesses configured".into();
            return;
        }
        self.mode = Mode::Dns;
        self.dns_pane = Some(spawn_dns_state(&self.config));
    }

    pub fn refresh_dns(&mut self) {
        if self.dns_pane.is_some() {
            self.dns_pane = Some(spawn_dns_state(&self.config));
        }
    }

    pub fn close_dns(&mut self) {
        if self.mode == Mode::Dns {
            self.dns_pane = None;
            self.mode = Mode::Browse;
        }
    }

    /// Re-fire every background fetch in one shot — vultr, buyvm, money,
    /// postmark, dns, health. Each underlying start_* guards its own
    /// preconditions (missing env var, empty businesses list), so this
    /// stays a single key away from the operator. Used by Browse `R`.
    pub fn refresh_all_overlays(&mut self) {
        self.start_vultr_fetch();
        self.start_buyvm_fetch();
        self.start_money_fetch();
        self.start_postmark_fetch();
        if !self.config.businesses.is_empty() {
            self.dns_pane = Some(spawn_dns_state(&self.config));
            self.health_pane = Some(spawn_health_state(&self.config.businesses));
        }
        self.status = "refreshing: vultr · buyvm · money · postmark · dns · health".into();
    }

    pub fn ingest_dns_events(&mut self) {
        let Some(s) = self.dns_pane.as_mut() else {
            return;
        };
        while let Ok(res) = s.rx.try_recv() {
            if let Some(slot) = s.rows.get_mut(res.idx) {
                *slot = Some(res.check);
            }
        }
    }

    /// Replay the selected history entry: jump Browse selection to the
    /// matching host (by `ssh_alias`), open the runner with the previous
    /// command pre-loaded in the input buffer, focus on Command so the
    /// operator can edit before pressing Enter. If no host matches the
    /// alias, set a status message and stay in History.
    pub fn replay_selected_history(&mut self) {
        let Some(state) = self.history_pane.as_ref() else {
            return;
        };
        let Some(entry) = state.entries.get(state.selected).cloned() else {
            return;
        };
        let Some(idx) = self
            .config
            .hosts
            .iter()
            .position(|h| h.ssh_alias == entry.alias)
        else {
            self.status = format!("no host with alias '{}'", entry.alias);
            return;
        };
        self.selected = idx;
        self.history_pane = None;
        self.mode = Mode::Runner;
        self.runner = RunnerState {
            input: entry.cmd.clone(),
            focus: Some(InputFocus::Command),
            scroll: ScrollState::new_sticky(),
            ..RunnerState::default()
        };
    }

    pub fn ingest_processes_events(&mut self) {
        let Some(s) = self.processes_pane.as_mut() else {
            return;
        };
        while let Ok(res) = s.rx.try_recv() {
            *s.slot_mut(res.slot) = Some(res.output);
        }
        s.try_compute();
    }

    pub fn open_shortcuts(&mut self) {
        if self.selected_host().is_none() {
            self.status = "no host selected".into();
            return;
        }
        if self.applicable_shortcuts().is_empty() {
            self.status = "no shortcuts for this host (config.toml)".into();
            return;
        }
        self.mode = Mode::Shortcuts;
    }

    pub fn close_shortcuts(&mut self) {
        if self.mode == Mode::Shortcuts {
            self.mode = Mode::Browse;
        }
    }

    /// Open the help overlay. Remembers the current mode so `close_help`
    /// returns to it. No-op when already in Help.
    pub fn open_help(&mut self) {
        if self.mode == Mode::Help {
            return;
        }
        self.help_origin = Some(self.mode);
        self.mode = Mode::Help;
    }

    pub fn close_help(&mut self) {
        if self.mode == Mode::Help {
            self.mode = self.help_origin.take().unwrap_or(Mode::Browse);
        }
    }

    pub fn applicable_shortcuts(&self) -> Vec<&crate::config::Shortcut> {
        let Some(host) = self.selected_host() else {
            return Vec::new();
        };
        self.config
            .shortcuts
            .iter()
            .filter(|s| s.applies_to(&host.ssh_alias))
            .collect()
    }

    /// Try to fire the shortcut bound to `key`. Returns true if a shortcut
    /// matched and a command was submitted to the Runner.
    pub fn fire_shortcut(&mut self, key: char) -> bool {
        let Some(host) = self.selected_host() else {
            return false;
        };
        let alias = host.ssh_alias.clone();
        let shortcut = self
            .config
            .shortcuts
            .iter()
            .find(|s| s.key == key && s.applies_to(&alias))
            .cloned();
        let Some(s) = shortcut else { return false };

        // Switch to Runner mode and submit the shortcut's command.
        self.mode = Mode::Runner;
        self.runner = RunnerState {
            focus: Some(InputFocus::Command),
            input: s.cmd.clone(),
            scroll: ScrollState::new_sticky(),
            ..RunnerState::default()
        };
        match crate::ssh::spawn_remote(&alias, &s.cmd) {
            Ok(handle) => {
                self.runner.output.push(OutputLine::System(format!(
                    "shortcut [{}] {} — $ ssh {alias} '{}'",
                    s.key, s.label, s.cmd
                )));
                self.run_handle = Some(handle);
                self.runner.running = true;
                self.runner.focus = None;
                self.runner.input.clear();
                self.runner.current_alias = Some(alias);
                self.runner.current_cmd = Some(s.cmd);
                self.runner.current_started_at = Some(Instant::now());
                self.runner.current_started_at_unix = Some(now_unix());
                true
            }
            Err(e) => {
                self.runner
                    .output
                    .push(OutputLine::System(format!("spawn failed: {e}")));
                false
            }
        }
    }

    pub fn ingest_services_events(&mut self) {
        let Some(s) = self.services.as_mut() else {
            return;
        };
        while let Ok(res) = s.rx.try_recv() {
            *s.slot_mut(res.slot) = Some(res.output);
        }
        s.try_compute();
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    pub fn ingest_run_events(&mut self) {
        // Drain via try_recv into a local buffer to release the immutable
        // borrow on `self.run_handle` before mutating self.
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(handle) = self.run_handle.as_ref() {
            loop {
                match handle.rx.try_recv() {
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
            self.apply_event(ev);
        }
        if disconnected {
            if self.runner.exit_code.is_none() {
                self.runner
                    .output
                    .push(OutputLine::System("(channel closed)".into()));
            }
            self.runner.running = false;
            self.run_handle = None;
        }
    }

    fn apply_event(&mut self, ev: RunEvent) {
        match ev {
            RunEvent::Out(l) => self.runner.output.push(OutputLine::Out(l)),
            RunEvent::Err(l) => self.runner.output.push(OutputLine::Err(l)),
            RunEvent::Partial(l) => self.runner.output.push(OutputLine::Partial(l)),
            RunEvent::NeedPassword => {
                self.runner.focus = Some(InputFocus::Password);
                self.runner.password.clear();
            }
            RunEvent::Done(code) => {
                self.runner.exit_code = Some(code);
                self.runner.running = false;
                self.runner
                    .output
                    .push(OutputLine::System(format!("exit {code}")));
                self.persist_operator_run(Some(code));
            }
            RunEvent::Error(msg) => {
                self.runner
                    .output
                    .push(OutputLine::System(format!("error: {msg}")));
                self.runner.running = false;
                self.persist_operator_run(None);
            }
        }
    }

    /// Persist the just-completed operator-driven run from `runner.output`
    /// to SQLite. Mirrors `persist_last_agent_entry` but the source is
    /// the in-memory RunnerState rather than the agent_history ring.
    /// Clears `runner.current_*` fields after a successful insert so a
    /// follow-up `[r] new cmd` doesn't double-persist.
    fn persist_operator_run(&mut self, exit: Option<i32>) {
        let Some(store) = self.history.as_mut() else {
            self.runner.current_alias = None;
            self.runner.current_cmd = None;
            self.runner.current_started_at = None;
            self.runner.current_started_at_unix = None;
            return;
        };
        let (Some(alias), Some(cmd), Some(start_unix)) = (
            self.runner.current_alias.clone(),
            self.runner.current_cmd.clone(),
            self.runner.current_started_at_unix,
        ) else {
            return;
        };
        let duration_ms = self
            .runner
            .current_started_at
            .and_then(|t| i64::try_from(t.elapsed().as_millis()).ok());
        let lines: Vec<LineRecord> = self
            .runner
            .output
            .iter()
            .map(output_line_to_record)
            .collect();
        if let Err(e) = store.insert_run(
            RunSource::Operator,
            &alias,
            &cmd,
            start_unix,
            exit,
            duration_ms,
            &lines,
        ) {
            tracing::warn!("history: insert_run(operator) failed: {e}");
        }
        self.runner.current_alias = None;
        self.runner.current_cmd = None;
        self.runner.current_started_at = None;
        self.runner.current_started_at_unix = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Business;

    fn biz(name: &str, stripe: Option<&str>, mercury: Option<&str>) -> Business {
        Business {
            name: name.into(),
            primary_domain: String::new(),
            host: String::new(),
            repo_path: String::new(),
            deploy_cmd: String::new(),
            notes: String::new(),
            stripe_account_id: stripe.map(String::from),
            mercury_account_id: mercury.map(String::from),
            postmark_server_token: None,
        }
    }

    fn app_with_businesses(bs: Vec<Business>) -> App {
        let cfg = Config {
            businesses: bs,
            ..Default::default()
        };
        App::new(cfg)
    }

    #[test]
    fn money_filter_skips_businesses_with_no_linkages() {
        let app = app_with_businesses(vec![
            biz("alpha", None, None),
            biz("beta", Some("acct_1"), None),
            biz("gamma", None, Some("acc-1")),
            biz("delta", None, None),
        ]);
        let eligible: Vec<&str> = app
            .money_filtered_businesses()
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(eligible, vec!["beta", "gamma"]);
    }

    #[test]
    fn cycle_money_filter_walks_eligible_then_wraps_to_none() {
        let mut app = app_with_businesses(vec![
            biz("alpha", Some("acct_1"), None),
            biz("beta", None, Some("acc-1")),
        ]);
        assert_eq!(app.money_filter, None);
        app.cycle_money_filter();
        assert_eq!(app.money_filter_business().map(|b| b.name.as_str()), Some("alpha"));
        app.cycle_money_filter();
        assert_eq!(app.money_filter_business().map(|b| b.name.as_str()), Some("beta"));
        app.cycle_money_filter();
        assert_eq!(app.money_filter, None);
    }

    #[test]
    fn cycle_money_filter_no_op_when_no_eligible() {
        let mut app = app_with_businesses(vec![biz("alpha", None, None)]);
        app.cycle_money_filter();
        assert_eq!(app.money_filter, None);
        assert!(!app.status.is_empty());
    }
}
