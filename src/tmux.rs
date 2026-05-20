//! Thin shell-out wrappers around tmux, either via ssh to a remote host or
//! against the operator's local tmux server.
//!
//! Sessions live on the chosen host. Each `target` parses as `alias[:label]`:
//!
//! - `vps1`         → ssh to `vps1`, tmux session name `helm`
//! - `vps1:deploy`  → ssh to `vps1`, tmux session name `helm-deploy`
//! - `local`        → operator's machine, tmux session name `helm`
//! - `local:claude` → operator's machine, tmux session name `helm-claude`
//!
//! The reserved alias `local` short-circuits ssh and runs tmux directly on
//! the operator's machine. Use cases: a shell that needs interactive doas
//! password entry, or a separate command history from the operator's own
//! shell. Each ssh'd host has its own tmux server; locally there's one.
//! Session names don't need to include the alias for uniqueness — the
//! alias picks the server, the label namespaces sessions within it.
//!
//! Sessions are created with `tmux new-session -A` (attach-or-create),
//! which is idempotent. `ensure_session` is a single fire-and-forget call.
//!
//! Why remote-tmux: persistence survives helm restarts AND network drops
//! AND operator-machine reboots. A remote session lives on the VPS until
//! the VPS itself reboots or someone runs `tmux kill-server` on it. A
//! `local` session lives until the operator's machine reboots or its
//! tmux server is killed.
//!
//! Text passed to `send_keys` is quoted for POSIX shell evaluation, so
//! passwords / shell metachars / spaces survive the round-trip.

use anyhow::{anyhow, Context, Result};
use std::process::{Command, Stdio};

/// Default scrollback lines for `capture`.
pub const DEFAULT_CAPTURE_LINES: u32 = 1000;

/// Reserved alias meaning "run tmux on the operator's machine, not via ssh".
pub const LOCAL_ALIAS: &str = "local";

/// Parse `alias[:label]` into `(alias, remote_session_name)`.
pub fn parse_target(target: &str) -> (String, String) {
    match target.split_once(':') {
        Some((alias, label)) if !label.is_empty() => {
            (alias.to_string(), format!("helm-{label}"))
        }
        Some((alias, _)) => (alias.to_string(), "helm".to_string()),
        None => (target.to_string(), "helm".to_string()),
    }
}

/// POSIX-shell-quote a string so it survives ssh's single round of remote
/// shell evaluation intact. Cheap allocation for the common case (typical
/// shell commands), but produces correct output for any UTF-8 input.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_=.,:@".contains(&b))
    {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Build the command that runs `script` on the host identified by `alias`.
/// For real ssh aliases this is `ssh <alias> -- <script>`; for the reserved
/// `local` alias it's `sh -c <script>`, skipping ssh entirely.
fn runner_cmd(alias: &str, script: &str) -> Command {
    if alias == LOCAL_ALIAS {
        let mut c = Command::new("sh");
        c.arg("-c").arg(script);
        c
    } else {
        let mut c = Command::new("ssh");
        c.arg(alias).arg(script);
        c
    }
}

/// Create the session detached if it doesn't already exist. `-A` makes
/// new-session attach-or-create, and `-d` keeps it detached so we don't
/// hijack stdin/stdout.
pub fn ensure_session(target: &str) -> Result<()> {
    let (alias, session) = parse_target(target);
    // `has-session || new-session -d` is the tty-less idempotent create.
    // The `-A` flag would be simpler but with `-d` it triggers tmux's
    // detach-client path, which needs a tty and fails over ssh with
    // `open terminal failed: not a terminal`. `-x`/`-y` set a default
    // pane size; the operator's later attach resizes to their real
    // terminal automatically.
    let q = shell_quote(&session);
    let remote = format!(
        "tmux has-session -t {q} 2>/dev/null || tmux new-session -d -x 200 -y 50 -s {q}"
    );
    let status = runner_cmd(&alias, &remote)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("spawn tmux new-session runner")?;
    if !status.success() {
        return Err(anyhow!(
            "`tmux new-session` on {alias} failed (exit {})",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Send a line of text (followed by Enter) to the session's active pane.
/// Text is split into two `send-keys` calls — first `-l` (literal, no
/// key-name parsing) for the body, then a bare `Enter`. This avoids every
/// escaping pitfall with characters tmux would otherwise interpret as key
/// names (e.g. text containing the word `Space` or `Enter`).
pub fn send_keys(target: &str, text: &str) -> Result<()> {
    let (alias, session) = parse_target(target);
    let q_session = shell_quote(&session);
    let q_text = shell_quote(text);
    let remote = format!(
        "tmux send-keys -t {q_session} -l {q_text} && tmux send-keys -t {q_session} Enter"
    );
    let status = runner_cmd(&alias, &remote)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("spawn tmux send-keys runner")?;
    if !status.success() {
        return Err(anyhow!(
            "`tmux send-keys` on {alias} failed (exit {})",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Capture the active pane's contents from the session.
pub fn capture(target: &str, lines: u32) -> Result<String> {
    let (alias, session) = parse_target(target);
    let q_session = shell_quote(&session);
    let neg = format!("-{lines}");
    let q_neg = shell_quote(&neg);
    let remote = format!("tmux capture-pane -t {q_session} -p -S {q_neg}");
    let out = runner_cmd(&alias, &remote)
        .output()
        .context("spawn tmux capture-pane runner")?;
    if !out.status.success() {
        return Err(anyhow!(
            "`tmux capture-pane` on {alias} failed (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// List helm-* sessions on the given alias's tmux server. Returns the
/// user-facing target form for each (e.g. `vps1`, `vps1:deploy`, `local`,
/// `local:claude`).
pub fn list(alias: &str) -> Result<Vec<String>> {
    let remote = "tmux list-sessions -F '#{session_name}' 2>/dev/null || true";
    let out = runner_cmd(alias, remote)
        .output()
        .context("spawn tmux list-sessions runner")?;
    if !out.status.success() {
        return Err(anyhow!(
            "tmux list-sessions on {alias} failed (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut targets = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let session = line.trim();
        if session == "helm" {
            targets.push(alias.to_string());
        } else if let Some(label) = session.strip_prefix("helm-") {
            targets.push(format!("{alias}:{label}"));
        }
    }
    Ok(targets)
}

/// Kill the session for `target`.
pub fn kill(target: &str) -> Result<()> {
    let (alias, session) = parse_target(target);
    let remote = format!("tmux kill-session -t {}", shell_quote(&session));
    let status = runner_cmd(&alias, &remote)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("spawn tmux kill-session runner")?;
    if !status.success() {
        return Err(anyhow!(
            "`tmux kill-session` on {alias} failed (exit {})",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_no_label_uses_default_session() {
        assert_eq!(parse_target("vps1"), ("vps1".into(), "helm".into()));
    }

    #[test]
    fn target_with_label_prefixes_helm() {
        assert_eq!(
            parse_target("vps1:deploy"),
            ("vps1".into(), "helm-deploy".into())
        );
    }

    #[test]
    fn target_with_empty_label_uses_default() {
        // `alias:` is treated as no label.
        assert_eq!(parse_target("vps1:"), ("vps1".into(), "helm".into()));
    }

    #[test]
    fn target_with_multiple_colons_takes_first_split() {
        // `alias:a:b` → label is `a:b`, session name `helm-a:b`. tmux
        // session names containing `:` are fine when properly quoted.
        assert_eq!(
            parse_target("vps1:a:b"),
            ("vps1".into(), "helm-a:b".into())
        );
    }

    #[test]
    fn shell_quote_passes_through_safe_strings() {
        assert_eq!(shell_quote("helm"), "helm");
        assert_eq!(shell_quote("uptime"), "uptime");
        assert_eq!(shell_quote("/var/log/messages"), "/var/log/messages");
        assert_eq!(shell_quote("helm-deploy"), "helm-deploy");
    }

    #[test]
    fn shell_quote_wraps_text_with_spaces_or_metachars() {
        assert_eq!(shell_quote("hello world"), "'hello world'");
        assert_eq!(shell_quote("doas rcctl restart httpd"), "'doas rcctl restart httpd'");
        assert_eq!(shell_quote("$VAR | rm -rf /"), "'$VAR | rm -rf /'");
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("'"), "''\\'''");
    }

    #[test]
    fn shell_quote_handles_empty() {
        assert_eq!(shell_quote(""), "''");
    }

    fn cmd_program(c: &Command) -> String {
        c.get_program().to_string_lossy().into_owned()
    }

    fn cmd_args(c: &Command) -> Vec<String> {
        c.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn runner_cmd_local_uses_sh_dash_c() {
        let c = runner_cmd(LOCAL_ALIAS, "tmux has-session -t helm");
        assert_eq!(cmd_program(&c), "sh");
        assert_eq!(cmd_args(&c), vec!["-c", "tmux has-session -t helm"]);
    }

    #[test]
    fn runner_cmd_remote_uses_ssh() {
        let c = runner_cmd("vps1", "tmux has-session -t helm");
        assert_eq!(cmd_program(&c), "ssh");
        assert_eq!(cmd_args(&c), vec!["vps1", "tmux has-session -t helm"]);
    }
}
