use std::io::{Read, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

/// Events emitted by a remote command in progress.
#[derive(Debug)]
pub enum RunEvent {
    /// One line of stdout (newline-terminated).
    Out(String),
    /// One line of stderr.
    Err(String),
    /// Partial line on stdout/stderr (not yet newline-terminated) — used for
    /// surfacing live state like `doas` password prompts that don't print a
    /// trailing newline.
    Partial(String),
    /// Detected a password prompt — UI should present a modal.
    NeedPassword,
    /// Process exited with given status code.
    Done(i32),
    /// IO error from the spawn/stream layer.
    Error(String),
}

/// Handle to a running remote command.
#[allow(dead_code)]
pub struct RunHandle {
    pub rx: Receiver<RunEvent>,
    stdin: Option<ChildStdin>,
    pub alias: String,
    pub command: String,
}

impl RunHandle {
    pub fn send_line(&mut self, line: &str) -> std::io::Result<()> {
        if let Some(s) = self.stdin.as_mut() {
            s.write_all(line.as_bytes())?;
            s.write_all(b"\n")?;
            s.flush()?;
        }
        Ok(())
    }
}

/// Spawn `ssh -tt <alias> <cmd>`, returning a handle that streams output
/// asynchronously via mpsc. `-tt` forces a remote PTY so `doas` writes its
/// password prompt to a fd we can see.
pub fn spawn_remote(alias: &str, cmd: &str) -> std::io::Result<RunHandle> {
    let mut child = Command::new("ssh")
        .arg("-tt")
        .arg(alias)
        .arg(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdin = child.stdin.take();

    let (tx, rx) = channel();

    {
        let tx = tx.clone();
        thread::spawn(move || stream(stdout, tx, false));
    }
    {
        let tx = tx.clone();
        thread::spawn(move || stream(stderr, tx, true));
    }
    thread::spawn(move || match child.wait() {
        Ok(s) => {
            let _ = tx.send(RunEvent::Done(s.code().unwrap_or(-1)));
        }
        Err(e) => {
            let _ = tx.send(RunEvent::Error(e.to_string()));
        }
    });

    Ok(RunHandle {
        rx,
        stdin,
        alias: alias.to_string(),
        command: cmd.to_string(),
    })
}

fn stream<R: Read + Send + 'static>(mut r: R, tx: Sender<RunEvent>, is_stderr: bool) {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut byte = [0u8; 1];
    let mut password_emitted = false;

    loop {
        match r.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                let b = byte[0];
                // Treat CR like LF — SSH PTY translates LF->CRLF on the way out.
                if b == b'\n' || b == b'\r' {
                    if !buf.is_empty() {
                        let line = String::from_utf8_lossy(&buf).into_owned();
                        let ev = if is_stderr {
                            RunEvent::Err(line)
                        } else {
                            RunEvent::Out(line)
                        };
                        let _ = tx.send(ev);
                        buf.clear();
                        password_emitted = false;
                    }
                } else {
                    buf.push(b);
                    if !password_emitted && looks_like_password_prompt(&buf) {
                        password_emitted = true;
                        let _ = tx.send(RunEvent::NeedPassword);
                        let partial = String::from_utf8_lossy(&buf).into_owned();
                        let _ = tx.send(RunEvent::Partial(partial));
                    }
                }
            }
            Err(_) => break,
        }
    }

    if !buf.is_empty() {
        let line = String::from_utf8_lossy(&buf).into_owned();
        let ev = if is_stderr {
            RunEvent::Err(line)
        } else {
            RunEvent::Out(line)
        };
        let _ = tx.send(ev);
    }
}

/// Cheap heuristic — password prompts end with ':' and contain "password" or
/// "passphrase" somewhere in the trailing chunk. doas, sudo, and ssh-key
/// passphrase prompts all match. False positives possible if a command echoes
/// the word literally, but the cost is one extra modal — acceptable for v1.
fn looks_like_password_prompt(buf: &[u8]) -> bool {
    if buf.len() < 9 {
        return false;
    }
    let tail_len = buf.len().min(96);
    let tail = &buf[buf.len() - tail_len..];
    let s = String::from_utf8_lossy(tail).to_ascii_lowercase();
    let trimmed = s.trim_end();
    if !trimmed.ends_with(':') {
        return false;
    }
    trimmed.contains("password") || trimmed.contains("passphrase")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_doas_prompt() {
        assert!(looks_like_password_prompt(b"doas (user) password:"));
        assert!(looks_like_password_prompt(b"doas (user@host) password: "));
        assert!(looks_like_password_prompt(b"[sudo] password for user:"));
    }

    #[test]
    fn ignores_regular_output() {
        assert!(!looks_like_password_prompt(b"hello world"));
        assert!(!looks_like_password_prompt(b"login: user"));
        assert!(!looks_like_password_prompt(b""));
    }
}
