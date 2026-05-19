//! Wire types for the helm control socket.

use serde::{Deserialize, Serialize};

/// Client → server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// Run an arbitrary command on a host. Output streams back as
    /// `Event::Out` / `Event::Err`, ending with `Event::Done { exit }`.
    Exec { alias: String, cmd: String },
}

/// Server → client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Out { line: String },
    Err { line: String },
    Done { exit: i32 },
    Error { msg: String },
}
