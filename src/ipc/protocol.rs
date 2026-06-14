//! Wire types for the helm control socket.

use serde::{Deserialize, Serialize};

/// Client → server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// Run an arbitrary command on a host. Output streams back as
    /// `Event::Out` / `Event::Err`, ending with `Event::Done { exit }`.
    Exec { alias: String, cmd: String },
    /// Liveness probe. Server replies with `Event::Pong { version }` then
    /// `Event::Done { exit: 0 }`.
    Ping,
    /// Ask the server to exit cleanly. Server replies with
    /// `Event::Done { exit: 0 }`, removes its socket file, and exits.
    Shutdown,
}

/// Server → client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Out { line: String },
    Err { line: String },
    Done { exit: i32 },
    Error { msg: String },
    Pong { version: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_roundtrip() {
        let r = Request::Ping;
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"kind":"ping"}"#);
        let back: Request = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, Request::Ping));
    }

    #[test]
    fn shutdown_roundtrip() {
        let r = Request::Shutdown;
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"kind":"shutdown"}"#);
        let back: Request = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, Request::Shutdown));
    }

    #[test]
    fn pong_roundtrip() {
        let e = Event::Pong {
            version: "0.1.0".into(),
        };
        let s = serde_json::to_string(&e).unwrap();
        let back: Event = serde_json::from_str(&s).unwrap();
        match back {
            Event::Pong { version } => assert_eq!(version, "0.1.0"),
            _ => panic!("wrong variant"),
        }
    }
}
