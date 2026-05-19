//! Inter-process control plane.
//!
//! When helm runs as a TUI it binds a Unix-domain socket at
//! `$XDG_RUNTIME_DIR/helm.sock` (fallback `$XDG_CACHE_HOME/helm/helm.sock`).
//! A separate helm invocation — `helm exec <alias> <cmd>` etc. — connects to
//! that socket, the TUI executes the request on the user's behalf, streams
//! results back to the client, and records the activity in the agent
//! history buffer so the human watching the TUI can see exactly what an
//! external operator is doing in real time.
//!
//! Line-delimited JSON protocol. One request per connection. Server emits
//! zero or more event lines, terminated by a `done` or `error` event, then
//! closes the connection.

pub mod client;
pub mod protocol;
pub mod server;

use std::path::PathBuf;

/// Resolve the socket path. Prefer `$XDG_RUNTIME_DIR/helm.sock` (per-user,
/// auto-cleaned at logout on systemd boxes; on OpenBSD `runtime_dir` is None
/// and we fall back to `$XDG_CACHE_HOME/helm/helm.sock`).
pub fn socket_path() -> PathBuf {
    if let Some(base) = directories::BaseDirs::new() {
        if let Some(rd) = base.runtime_dir() {
            return rd.join("helm.sock");
        }
        let cache = base.cache_dir().join("helm");
        let _ = std::fs::create_dir_all(&cache);
        return cache.join("helm.sock");
    }
    std::env::temp_dir().join("helm.sock")
}
