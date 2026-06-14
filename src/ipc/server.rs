//! Unix-socket server that bridges client requests into the engine.
//!
//! Threading model:
//! - One accept thread holds the `UnixListener`. For each incoming connection
//!   it spawns a worker thread.
//! - The worker reads one `Request` from the socket.
//!   - `Ping` and `Shutdown` are handled inline by the worker (no Job).
//!   - `Exec` allocates an mpsc pair (`response_tx`/`response_rx`), packages
//!     everything into a `Job`, and sends it to the engine via the global
//!     jobs channel. The worker then loops reading `Event`s off
//!     `response_rx` and writing them as JSON lines until the channel
//!     disconnects.
//!
//! The engine drains pending jobs each tick, executes them through the
//! existing `ssh::spawn_remote` machinery, and forwards every RunEvent to
//! both the agent history buffer (so the TUI can render it) AND the job's
//! `response_tx` (so the client sees the same stream).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use crate::ipc::protocol::{Event, Request};

/// One unit of work the server hands to the engine.
pub struct Job {
    pub request: Request,
    /// Engine pushes Events here; server worker forwards them to its
    /// client socket. Dropping this Sender (e.g. after a `Done` event)
    /// signals the worker that no more output is coming.
    pub response_tx: Sender<Event>,
}

/// Guard that owns the socket file and removes it on drop.
pub struct SocketGuard {
    pub socket_path: PathBuf,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Handles returned from `start`. The caller (TUI or daemon) holds these:
/// - `guard` keeps the socket file alive (removed on drop)
/// - `jobs_rx` is polled by the engine for new Exec requests
/// - `shutdown_rx` fires once when a `Request::Shutdown` arrives; daemon
///   should use this to exit its main loop. The TUI can ignore it (it
///   manages lifetime through its own quit key).
pub struct ServerHandles {
    pub guard: SocketGuard,
    pub jobs_rx: Receiver<Job>,
    pub shutdown_rx: Receiver<()>,
}

/// Start the server. If a stale socket exists at `path` we remove it and
/// rebind.
pub fn start(path: PathBuf) -> std::io::Result<ServerHandles> {
    // Best-effort: remove a stale socket from a previous run that exited
    // without cleaning up. Only safe because we then try to bind; if some
    // other helm process is actively bound to it, our bind would have failed
    // even before this — but on Unix unlinking a bound socket only removes
    // the dirent, the kernel keeps the bound fd live for the running peer.
    // The new bind will succeed and the old peer keeps running on its old fd.
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path)?;
    // 0600 — only the user can open the socket.
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

    let (jobs_tx, jobs_rx) = channel::<Job>();
    let (shutdown_tx, shutdown_rx) = channel::<()>();

    thread::spawn(move || accept_loop(listener, jobs_tx, shutdown_tx));

    Ok(ServerHandles {
        guard: SocketGuard { socket_path: path },
        jobs_rx,
        shutdown_rx,
    })
}

fn accept_loop(listener: UnixListener, jobs_tx: Sender<Job>, shutdown_tx: Sender<()>) {
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let tx = jobs_tx.clone();
                let stx = shutdown_tx.clone();
                thread::spawn(move || {
                    if let Err(e) = handle_client(stream, tx, stx) {
                        tracing::warn!("ipc client failed: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("accept failed: {e}");
                // brief backoff to avoid a tight error loop
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

fn handle_client(
    stream: UnixStream,
    jobs_tx: Sender<Job>,
    shutdown_tx: Sender<()>,
) -> std::io::Result<()> {
    let writer_stream = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim().is_empty() {
        return Ok(());
    }

    let request: Request = match serde_json::from_str(line.trim()) {
        Ok(r) => r,
        Err(e) => {
            return write_event(
                writer_stream,
                &Event::Error {
                    msg: format!("malformed request: {e}"),
                },
            );
        }
    };

    match request {
        Request::Ping => {
            write_event(
                writer_stream.try_clone()?,
                &Event::Pong {
                    version: env!("CARGO_PKG_VERSION").into(),
                },
            )?;
            write_event(writer_stream, &Event::Done { exit: 0 })
        }
        Request::Shutdown => {
            write_event(writer_stream, &Event::Done { exit: 0 })?;
            // best-effort signal to the engine loop; ignore if no one's listening
            let _ = shutdown_tx.send(());
            Ok(())
        }
        Request::Exec { .. } => {
            let (response_tx, response_rx) = channel::<Event>();
            let job = Job {
                request,
                response_tx,
            };
            if jobs_tx.send(job).is_err() {
                return write_event(
                    writer_stream,
                    &Event::Error {
                        msg: "helm engine unreachable".into(),
                    },
                );
            }
            forward_events(writer_stream, response_rx)
        }
    }
}

fn write_event(mut stream: UnixStream, event: &Event) -> std::io::Result<()> {
    let line = serde_json::to_string(event).map_err(std::io::Error::other)?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

fn forward_events(mut stream: UnixStream, response_rx: Receiver<Event>) -> std::io::Result<()> {
    while let Ok(ev) = response_rx.recv() {
        let line = serde_json::to_string(&ev).map_err(std::io::Error::other)?;
        if stream.write_all(line.as_bytes()).is_err() {
            // client disconnected; abandon further writes
            break;
        }
        if stream.write_all(b"\n").is_err() {
            break;
        }
        let _ = stream.flush();
    }
    Ok(())
}
