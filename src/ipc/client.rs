//! Connect to a running helm daemon (or TUI) control socket and stream
//! events. Used by `helm exec`, `helm daemon stop`, `helm daemon status`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use crate::ipc::protocol::{Event, Request};
use crate::ipc::socket_path;

/// Connect, send `request`, stream events to stdout/stderr, return an
/// ExitCode reflecting the remote process exit (or 1 on error).
pub fn run(request: &Request) -> ExitCode {
    let path = socket_path();
    match connect_and_stream(&path, request) {
        Ok(code) => exit_from(code),
        Err(e) => {
            eprintln!("helm: {e}");
            ExitCode::from(1)
        }
    }
}

fn exit_from(code: i32) -> ExitCode {
    let c: u8 = if (0..=255).contains(&code) {
        code as u8
    } else {
        1
    };
    ExitCode::from(c)
}

fn connect_and_stream(path: &Path, request: &Request) -> anyhow::Result<i32> {
    let mut stream = UnixStream::connect(path).map_err(|e| {
        anyhow::anyhow!(
            "cannot reach helm daemon at {} ({e}). Start it with `helm daemon` or open the TUI.",
            path.display()
        )
    })?;

    let line = serde_json::to_string(request)?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let reader = BufReader::new(stream);
    let mut exit_code: i32 = 1;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let ev: Event = serde_json::from_str(&line)?;
        match ev {
            Event::Out { line } => println!("{line}"),
            Event::Err { line } => eprintln!("{line}"),
            Event::Done { exit } => {
                exit_code = exit;
            }
            Event::Error { msg } => {
                eprintln!("helm: {msg}");
                exit_code = 1;
            }
            // Pong is consumed silently by `ping_socket` (which uses
            // collect_events). When forwarded through `run`, just ignore.
            Event::Pong { .. } => {}
        }
    }

    Ok(exit_code)
}

/// Probe whether a helm process is listening on the socket. Returns
/// `Ok(Some(version))` if the daemon (or TUI) is up, `Ok(None)` if the
/// socket is not reachable (file missing or refusing connections), or
/// `Err` for any other I/O / protocol failure.
pub fn ping_socket(path: &Path) -> anyhow::Result<Option<String>> {
    let mut stream = match UnixStream::connect(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
            || e.kind() == std::io::ErrorKind::ConnectionRefused =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let req = serde_json::to_string(&Request::Ping)?;
    stream.write_all(req.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let reader = BufReader::new(stream);
    let mut version: Option<String> = None;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let ev: Event = serde_json::from_str(&line)?;
        match ev {
            Event::Pong { version: v } => version = Some(v),
            Event::Done { .. } => break,
            _ => {}
        }
    }
    Ok(version)
}

/// Send `Shutdown` to the daemon, then poll for the socket file to
/// disappear (up to `wait`). Returns true if the daemon went away.
pub fn shutdown_socket(path: &Path, wait: Duration) -> anyhow::Result<bool> {
    let mut stream = match UnixStream::connect(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
            || e.kind() == std::io::ErrorKind::ConnectionRefused =>
        {
            return Ok(true);
        }
        Err(e) => return Err(e.into()),
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let req = serde_json::to_string(&Request::Shutdown)?;
    stream.write_all(req.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    // Drain the response (Done) but don't require it — daemon may close
    // the socket before we finish reading.
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        if line.is_err() {
            break;
        }
    }

    let deadline = std::time::Instant::now() + wait;
    while std::time::Instant::now() < deadline {
        if !path.exists() {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(!path.exists())
}
