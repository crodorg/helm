//! Thin shell-out wrappers around tmux, either via ssh to a remote host or
//! against the operator's local tmux server.
//!
//! Sessions live on the chosen host. Each `target` parses as `alias[:label]`:
//!
//! - `vps1`         → ssh to `vps1`, tmux session name `helm`
//! - `vps1:deploy`  → ssh to `vps1`, tmux session name `helm-deploy`
//! - `local`        → operator's machine, tmux session name `helm`
//! - `local:agent`  → operator's machine, tmux session name `helm-agent`
//!
//! The reserved alias `local` short-circuits ssh and runs tmux directly on
//! the operator's machine. Use cases: a shell that needs interactive doas/sudo
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

use anyhow::{Context, Result, anyhow};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

/// Default scrollback lines for `read` (the agent-facing capture). Kept low
/// because a headless helm pane is 50 rows tall and a routine read is mostly
/// blank padding — see `strip_trailing_blank`. Long logs pass an explicit
/// `-n`.
pub const DEFAULT_CAPTURE_LINES: u32 = 200;

/// Default seconds `run` polls for its sentinel before returning the command
/// as still-running.
pub const DEFAULT_RUN_TIMEOUT_SECS: u32 = 30;

/// Monotonic per-process counter so concurrent `run` calls get distinct
/// sentinels without consulting a wall clock (unavailable in some build/test
/// contexts). Paired with the pid, it makes the sentinel collision-proof in
/// practice.
static RUN_SEQ: AtomicU32 = AtomicU32::new(0);

/// Reserved alias meaning "run tmux on the operator's machine, not via ssh".
pub const LOCAL_ALIAS: &str = "local";

/// Process-global flags inserted after `tmux` on every invocation helm makes
/// (e.g. `-u` to force UTF-8). Set once at startup from config via
/// [`set_flags`]; unset means plain `tmux`. A global keeps the tmux helpers'
/// signatures unchanged — config is loaded once per process, so a single
/// set is enough for the CLI, the TUI, and the daemon alike.
static TMUX_FLAGS: OnceLock<Vec<String>> = OnceLock::new();

/// Install the global tmux flags. First call wins (OnceLock); later calls are
/// no-ops, which is fine since each helm process loads config exactly once.
pub fn set_flags(flags: Vec<String>) {
    let _ = TMUX_FLAGS.set(flags);
}

/// The configured flags, or an empty slice if [`set_flags`] was never called.
/// Used by the local attach path, which builds a `Command` and pushes args
/// directly rather than embedding a shell script.
pub fn flags() -> Vec<String> {
    TMUX_FLAGS.get().cloned().unwrap_or_default()
}

/// The `tmux` command word plus configured global flags, shell-quoted and
/// space-joined for embedding in a remote script — e.g. `tmux -u`. Split out
/// as a pure helper for testing the global-free path.
fn build_tmux_prefix(flags: &[String]) -> String {
    let mut s = String::from("tmux");
    for f in flags {
        s.push(' ');
        s.push_str(&shell_quote(f));
    }
    s
}

/// `tmux` with the process-global flags applied (see [`set_flags`]).
pub fn tmux_prefix() -> String {
    build_tmux_prefix(TMUX_FLAGS.get().map(Vec::as_slice).unwrap_or(&[]))
}

/// Parse `alias[:label]` into `(alias, remote_session_name)`.
pub fn parse_target(target: &str) -> (String, String) {
    match target.split_once(':') {
        Some((alias, label)) if !label.is_empty() => (alias.to_string(), format!("helm-{label}")),
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

/// Prefix common Homebrew + MacPorts directories onto `$PATH` for the
/// remote (or local-via-`sh -c`) shell. Non-interactive ssh sessions on
/// macOS skip `.zshrc` and never pick up `/opt/homebrew/bin`, so a stock
/// `ssh <laptop> tmux ...` fails with `tmux: command not found` even after
/// `brew install tmux`. Uses `export` (not the `KEY=value cmd` prefix
/// form) so compound scripts like `tmux has-session || tmux new-session`
/// see the augmented PATH on every branch. Idempotent on hosts that
/// already have these dirs in `PATH`.
pub fn with_remote_path(script: &str) -> String {
    format!("export PATH=\"$PATH:/opt/homebrew/bin:/usr/local/bin:/opt/local/bin\"; {script}")
}

/// Build the command that runs `script` on the host identified by `alias`.
/// For real ssh aliases this is `ssh <alias> -- <script>`; for the reserved
/// `local` alias it's `sh -c <script>`, skipping ssh entirely. The script
/// is always wrapped with `with_remote_path` so brew-installed tools
/// resolve even in non-interactive ssh shells.
fn runner_cmd(alias: &str, script: &str) -> Command {
    let script = with_remote_path(script);
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
    let tmux = tmux_prefix();
    let remote = format!(
        "{tmux} has-session -t {q} 2>/dev/null || {tmux} new-session -d -x 200 -y 50 -s {q}"
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
    let tmux = tmux_prefix();
    // `--` ends tmux's option parsing so a body starting with `-` (e.g.
    // `--help`) is treated as literal keys, not a `send-keys` flag.
    let remote = format!(
        "{tmux} send-keys -t {q_session} -l -- {q_text} && {tmux} send-keys -t {q_session} Enter"
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

/// Send raw tmux key specs to the session's active pane — no literal `-l`
/// flag and no trailing Enter. This is the TUI-driving path: each `key` is a
/// tmux key name (`Up`, `C-c`, `Enter`, `Escape`, `F1`, …) passed straight
/// through, so the agent can operate a full-screen program (vim, htop, a
/// menu) on a remote host, which the line-oriented `send_keys` cannot do.
/// `--` ends option parsing so a key spec beginning with `-` is never
/// mistaken for a `send-keys` flag.
pub fn send_raw_keys(target: &str, keys: &[String]) -> Result<()> {
    if keys.is_empty() {
        return Err(anyhow!("no keys to send"));
    }
    let (alias, session) = parse_target(target);
    let q_session = shell_quote(&session);
    let joined = keys
        .iter()
        .map(|k| shell_quote(k))
        .collect::<Vec<_>>()
        .join(" ");
    let remote = format!("{} send-keys -t {q_session} -- {joined}", tmux_prefix());
    let status = runner_cmd(&alias, &remote)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("spawn tmux send-keys (raw) runner")?;
    if !status.success() {
        return Err(anyhow!(
            "`tmux send-keys` (raw) on {alias} failed (exit {})",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Outcome of a `helm shell run`: the command's own output and exit status,
/// parsed from the sentinel the wrapper prints into the pane.
#[derive(Debug, PartialEq, Eq)]
pub struct RunOutcome {
    /// The command's output (trailing blank lines stripped).
    pub output: String,
    /// `Some(code)` once the sentinel is seen; `None` means the command was
    /// still running when the poll timed out.
    pub exit: Option<i32>,
    /// True when the pane was busy (not at a settled prompt) so nothing was
    /// sent — the caller should fall back to `read`/`send`.
    pub busy: bool,
    /// True when the command terminated the shell (e.g. `exit`/`logout`),
    /// destroying the session — so no sentinel could be printed. Distinct
    /// from a timeout: the shell is gone, not still working.
    pub gone: bool,
}

/// Build the per-call sentinel tag. Split out so the parser tests can mint
/// the same shape the wrapper uses.
fn run_tag() -> String {
    let pid = std::process::id();
    let seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("__helm_{pid}_{seq}")
}

/// Stateful one-shot: ensure the session, type `cmd` followed by a sentinel
/// `printf`, then poll for the sentinel — *all in a single remote `sh`
/// invocation*, so the poll loop runs on the host rather than as repeated ssh
/// round-trips. Returns the command's output and exit code. `cmd` must be a
/// single line (callers reject embedded newlines, which would detach the
/// sentinel from the command). Non-interactive commands only; a pane sitting
/// in a pager/editor is reported as `busy`.
pub fn run_command(target: &str, cmd: &str, timeout_secs: u32) -> Result<RunOutcome> {
    let (alias, session) = parse_target(target);
    let q_session = shell_quote(&session);
    let tmux = tmux_prefix();
    let tag = run_tag();
    // The line typed into the shell: the command, then a printf emitting
    // `<tag>:<exit>:` on its own line. `;` so the printf always runs and `$?`
    // reflects the command's own exit. The leading `\n` guarantees the
    // sentinel starts a fresh line even if the command's output lacks a
    // trailing newline.
    let payload = format!("{cmd}; printf '\\n{tag}:%d:\\n' \"$?\"");
    let q_payload = shell_quote(&payload);
    // Match the *result* sentinel (`<tag>:` then a digit), never the echoed
    // printf format (`<tag>:%d`), so the poll can't exit early on the input
    // echo line.
    let q_grep = shell_quote(&format!("{tag}:[0-9]"));
    let q_busy = shell_quote(&format!("{tag}:busy:"));
    let q_gone = shell_quote(&format!("{tag}:gone:"));
    let iters = timeout_secs.max(1) * 5; // 0.2s poll period
    let script = build_run_script(
        &tmux, &q_session, &q_payload, &q_grep, &q_busy, &q_gone, iters,
    );
    let out = runner_cmd(&alias, &script)
        .output()
        .context("spawn tmux run runner")?;
    if !out.status.success() {
        return Err(anyhow!(
            "`helm shell run` on {alias} failed (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let captured = String::from_utf8_lossy(&out.stdout).into_owned();
    Ok(extract_run(&captured, &tag))
}

/// Assemble the single remote `sh` script `run_command` ships: ensure the
/// session, busy-guard at a prompt, send the payload, poll for the sentinel
/// (host-side, so one ssh round-trip), and emit the final capture. Split out as
/// a pure builder so the script's shape is unit-tested without a tmux server.
/// All `q_*` args are already shell-quoted by the caller.
#[allow(clippy::too_many_arguments)]
fn build_run_script(
    tmux: &str,
    q_session: &str,
    q_payload: &str,
    q_grep: &str,
    q_busy: &str,
    q_gone: &str,
    iters: u32,
) -> String {
    format!(
        "{tmux} has-session -t {q_session} 2>/dev/null || \
           {tmux} new-session -d -x 200 -y 50 -s {q_session}; \
         sleep 0.3; \
         __l=$({tmux} capture-pane -t {q_session} -p -S -20 | grep . | tail -1); \
         case \"$__l\" in ''|*'$'|*'#'|*'%') ;; *) printf '%s\\n' {q_busy}; exit 0 ;; esac; \
         {tmux} send-keys -t {q_session} -l -- {q_payload}; \
         {tmux} send-keys -t {q_session} Enter; \
         __i=0; while [ $__i -lt {iters} ]; do \
           {tmux} has-session -t {q_session} 2>/dev/null || break; \
           {tmux} capture-pane -t {q_session} -p -S -500 | grep -E -q -e {q_grep} && break; \
           sleep 0.2; __i=$(( __i + 1 )); \
         done; \
         {tmux} has-session -t {q_session} 2>/dev/null || {{ printf '%s\\n' {q_gone}; exit 0; }}; \
         {tmux} capture-pane -t {q_session} -p -S -500"
    )
}

/// Pure parser: pull the command's output and exit code out of the wrapper's
/// final pane capture. Kept free of I/O so the sentinel-extraction logic is
/// unit-tested directly. See `run_command` for the sentinel shape.
pub fn extract_run(captured: &str, tag: &str) -> RunOutcome {
    let busy_marker = format!("{tag}:busy:");
    if captured.lines().any(|l| l.trim_end() == busy_marker) {
        return RunOutcome {
            output: String::new(),
            exit: None,
            busy: true,
            gone: false,
        };
    }
    let gone_marker = format!("{tag}:gone:");
    if captured.lines().any(|l| l.trim_end() == gone_marker) {
        return RunOutcome {
            output: String::new(),
            exit: None,
            busy: false,
            gone: true,
        };
    }
    let lines: Vec<&str> = captured.lines().collect();
    let echo_needle = format!("{tag}:%d"); // the typed printf format
    match lines.iter().rposition(|l| sentinel_exit(l, tag).is_some()) {
        Some(s_idx) => {
            let exit = sentinel_exit(lines[s_idx], tag);
            let echo_idx = lines[..s_idx]
                .iter()
                .rposition(|l| l.contains(&echo_needle));
            let start = echo_idx.map(|i| i + 1).unwrap_or(0);
            RunOutcome {
                output: strip_trailing_blank(&lines[start..s_idx].join("\n")),
                exit,
                busy: false,
                gone: false,
            }
        }
        None => {
            // No sentinel — command still running at timeout. Best-effort:
            // everything after the echoed input line.
            let echo_idx = lines.iter().rposition(|l| l.contains(&echo_needle));
            let start = echo_idx.map(|i| i + 1).unwrap_or(0);
            RunOutcome {
                output: strip_trailing_blank(&lines[start..].join("\n")),
                exit: None,
                busy: false,
                gone: false,
            }
        }
    }
}

/// If `line` is exactly the result sentinel `<tag>:<digits>:`, return the
/// parsed exit code. Trailing whitespace (capture-pane never adds any, but be
/// safe) is ignored.
fn sentinel_exit(line: &str, tag: &str) -> Option<i32> {
    let digits = line
        .trim_end()
        .strip_prefix(tag)?
        .strip_prefix(':')?
        .strip_suffix(':')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Strip trailing blank (empty or whitespace-only) lines. Drops tmux's
/// pane-height padding from a `read` and the sentinel printf's trailing blank
/// from a `run`. Leading blanks are left intact — in copy-mode they encode
/// the frozen viewport's scroll position.
pub fn strip_trailing_blank(s: &str) -> String {
    let mut lines: Vec<&str> = s.lines().collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// Capture the active pane's contents from the session.
pub fn capture(target: &str, lines: u32) -> Result<String> {
    let (alias, session) = parse_target(target);
    let q_session = shell_quote(&session);
    let neg = format!("-{lines}");
    let q_neg = shell_quote(&neg);
    let remote = format!(
        "{} capture-pane -t {q_session} -p -S {q_neg}",
        tmux_prefix()
    );
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
/// `local:agent`).
pub fn list(alias: &str) -> Result<Vec<String>> {
    let remote = format!(
        "{} list-sessions -F '#{{session_name}}' 2>/dev/null || true",
        tmux_prefix()
    );
    let out = runner_cmd(alias, &remote)
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
    let remote = format!(
        "{} kill-session -t {}",
        tmux_prefix(),
        shell_quote(&session)
    );
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
        assert_eq!(parse_target("vps1:a:b"), ("vps1".into(), "helm-a:b".into()));
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
        assert_eq!(
            shell_quote("doas rcctl restart httpd"),
            "'doas rcctl restart httpd'"
        );
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
        let args = cmd_args(&c);
        assert_eq!(args[0], "-c");
        assert!(args[1].contains("tmux has-session -t helm"));
        assert!(args[1].contains("/opt/homebrew/bin"));
    }

    #[test]
    fn runner_cmd_remote_uses_ssh() {
        let c = runner_cmd("vps1", "tmux has-session -t helm");
        assert_eq!(cmd_program(&c), "ssh");
        let args = cmd_args(&c);
        assert_eq!(args[0], "vps1");
        assert!(args[1].contains("tmux has-session -t helm"));
        assert!(args[1].contains("/opt/homebrew/bin"));
    }

    #[test]
    fn build_tmux_prefix_empty_is_bare_tmux() {
        assert_eq!(build_tmux_prefix(&[]), "tmux");
    }

    #[test]
    fn build_tmux_prefix_appends_flags_shell_quoted() {
        assert_eq!(build_tmux_prefix(&["-u".into()]), "tmux -u");
        assert_eq!(build_tmux_prefix(&["-u".into(), "-2".into()]), "tmux -u -2");
        // A flag with a space/metachar gets quoted so it survives the
        // remote shell intact.
        assert_eq!(
            build_tmux_prefix(&["-L".into(), "my socket".into()]),
            "tmux -L 'my socket'"
        );
    }

    #[test]
    fn with_remote_path_exports_before_script() {
        let s = with_remote_path("tmux a || tmux b");
        // `export` (not the `KEY=val cmd` prefix form) so both branches
        // of a compound script inherit the augmented PATH.
        assert!(s.starts_with("export PATH="));
        assert!(s.contains("/opt/homebrew/bin"));
        assert!(s.contains("tmux a || tmux b"));
        assert!(s.contains("; tmux a"));
    }

    #[test]
    fn strip_trailing_blank_drops_pane_padding() {
        assert_eq!(strip_trailing_blank("a\nb\n\n\n"), "a\nb");
        assert_eq!(strip_trailing_blank("a\n  \n\t\n"), "a");
        assert_eq!(strip_trailing_blank("\n\n"), "");
        assert_eq!(strip_trailing_blank(""), "");
        // Internal and leading blanks are preserved.
        assert_eq!(strip_trailing_blank("\na\n\nb"), "\na\n\nb");
    }

    #[test]
    fn sentinel_exit_matches_only_the_result_line() {
        let tag = "__helm_1_0";
        assert_eq!(sentinel_exit("__helm_1_0:0:", tag), Some(0));
        assert_eq!(sentinel_exit("__helm_1_0:127:", tag), Some(127));
        // The echoed printf format is not a result (has `%d`, not digits).
        assert_eq!(sentinel_exit("__helm_1_0:%d:", tag), None);
        // Surrounding text disqualifies it (only an exact line counts).
        assert_eq!(sentinel_exit("$ foo __helm_1_0:0:", tag), None);
        assert_eq!(sentinel_exit("hello", tag), None);
        assert_eq!(sentinel_exit("__helm_1_0:busy:", tag), None);
    }

    #[test]
    fn extract_run_pulls_output_and_exit() {
        let tag = "__helm_1_0";
        let cap = "-bash-5.3$ echo hello; printf '\\n__helm_1_0:%d:\\n' \"$?\"\n\
                   hello\n\n__helm_1_0:0:\n-bash-5.3$\n\n\n";
        let out = extract_run(cap, tag);
        assert_eq!(out.output, "hello");
        assert_eq!(out.exit, Some(0));
        assert!(!out.busy);
    }

    #[test]
    fn extract_run_reports_nonzero_exit() {
        let tag = "__helm_1_0";
        let cap = "$ false; printf '\\n__helm_1_0:%d:\\n' \"$?\"\n\n__helm_1_0:1:\n$\n";
        let out = extract_run(cap, tag);
        assert_eq!(out.output, "");
        assert_eq!(out.exit, Some(1));
    }

    #[test]
    fn extract_run_detects_busy() {
        let out = extract_run("__helm_1_0:busy:\n", "__helm_1_0");
        assert!(out.busy);
        assert_eq!(out.exit, None);
        assert_eq!(out.output, "");
    }

    #[test]
    fn build_run_script_has_the_expected_shape() {
        let s = build_run_script(
            "tmux -u",
            "helm",
            "'payload'",
            "'t:[0-9]'",
            "'t:busy:'",
            "'t:gone:'",
            150,
        );
        assert!(s.contains("has-session -t helm"));
        assert!(s.contains("new-session -d -x 200 -y 50 -s helm"));
        assert!(s.contains("send-keys -t helm -l -- 'payload'"));
        assert!(s.contains("send-keys -t helm Enter"));
        assert!(s.contains("grep -E -q -e 't:[0-9]'"));
        assert!(s.contains("printf '%s\\n' 't:busy:'"));
        assert!(s.contains("printf '%s\\n' 't:gone:'"));
        assert!(s.contains("[ $__i -lt 150 ]"));
        assert!(s.contains("capture-pane -t helm -p -S -500"));
    }

    #[test]
    fn extract_run_detects_gone_session() {
        let out = extract_run("__helm_1_0:gone:\n", "__helm_1_0");
        assert!(out.gone);
        assert!(!out.busy);
        assert_eq!(out.exit, None);
    }

    #[test]
    fn extract_run_timeout_has_no_exit() {
        // No result sentinel — command still running. Output is everything
        // after the echoed input line.
        let tag = "__helm_1_0";
        let cap = "$ sleep 99; printf '\\n__helm_1_0:%d:\\n' \"$?\"\nstill working\n";
        let out = extract_run(cap, tag);
        assert_eq!(out.exit, None);
        assert!(!out.busy);
        assert_eq!(out.output, "still working");
    }

    #[test]
    fn extract_run_without_echo_line_takes_from_start() {
        // No echoed printf-format line present — output runs from the top of
        // the capture to the sentinel.
        let out = extract_run("hello\nworld\n__helm_1_0:0:\n", "__helm_1_0");
        assert_eq!(out.output, "hello\nworld");
        assert_eq!(out.exit, Some(0));
    }

    #[test]
    fn extract_run_takes_the_last_sentinel() {
        // A prior run's sentinel is still in scrollback; the most recent one
        // (and the echo just before it) is what counts.
        let tag = "__helm_1_0";
        let cap = "$ a; printf '\\n__helm_1_0:%d:\\n' \"$?\"\nold\n__helm_1_0:9:\n\
                   $ b; printf '\\n__helm_1_0:%d:\\n' \"$?\"\nnew\n__helm_1_0:0:\n$\n";
        let out = extract_run(cap, tag);
        assert_eq!(out.output, "new");
        assert_eq!(out.exit, Some(0));
    }
}
