//! Connect to a running helm TUI's control socket and stream events to
//! stdout / stderr. Used by the `helm exec` subcommand.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;

use crate::ipc::protocol::{Event, Request};
use crate::ipc::socket_path;

/// Connect to the TUI, send `request`, stream events to stdout/stderr, and
/// return an ExitCode reflecting the remote process exit (or 1 on error).
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
    // ExitCode only accepts u8 (POSIX). Clamp.
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
            "cannot reach helm TUI at {} ({e}). Start `helm` in another terminal first.",
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
        }
    }

    Ok(exit_code)
}
