//! Append-only audit log of agent-driven helm actions.
//!
//! Every CLI invocation that an external agent could plausibly trigger
//! (`helm exec`, `helm shell open/run/send/key/read/list/close`, and the
//! same `helm pane` verbs — logged with alias `pane`) writes one JSON
//! line to `$XDG_STATE_HOME/helm/activity.jsonl` (or `~/.local/state/helm/`
//! on Linux, `~/Library/Application Support/helm/` on macOS). `helm activity`
//! reads back the most recent records; the operator can also `tail -f` the
//! file directly to watch what the agent is doing in real time.
//!
//! Design constraints:
//!
//! - **Append-only on disk.** Writes use `O_APPEND` + a single short write
//!   per record so concurrent helm processes can't corrupt each other and
//!   the log is tamper-resistant from inside the helm process itself.
//! - **Agent-agnostic.** Logged from the helm CLI, not the agent. Whether
//!   the caller is Claude / Cursor / Aider / a bash one-liner, the same
//!   record gets written.
//! - **Cheap.** Best-effort. If logging fails the command still runs;
//!   the operator gets a stderr warning but never an error exit code.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Exec,
    ShellOpen,
    ShellSend,
    ShellRun,
    ShellWait,
    ShellWatch,
    ShellKey,
    ShellRead,
    ShellList,
    ShellClose,
}

impl ActivityKind {
    pub fn label(self) -> &'static str {
        match self {
            ActivityKind::Exec => "exec",
            ActivityKind::ShellOpen => "open",
            ActivityKind::ShellSend => "send",
            ActivityKind::ShellRun => "run",
            ActivityKind::ShellWait => "wait",
            ActivityKind::ShellWatch => "watch",
            ActivityKind::ShellKey => "key",
            ActivityKind::ShellRead => "read",
            ActivityKind::ShellList => "list",
            ActivityKind::ShellClose => "close",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityRecord {
    pub ts_unix: u64,
    pub pid: u32,
    pub ppid: u32,
    pub kind: ActivityKind,
    pub alias: String,
    /// tmux label component (`alias:label`) or empty for bare `<alias>` /
    /// non-tmux commands.
    pub session: String,
    /// The command text the agent asked helm to run / send.
    pub cmd: String,
    /// First two non-empty lines of the captured output, joined with `⏎`.
    /// Always empty for kinds that don't read output.
    pub output_preview: String,
    /// True when `cmd` mentions `doas` or `sudo` as a whole-word token —
    /// `helm activity` flags these so privilege escalation is easy to spot.
    pub has_privilege_escalation: bool,
    /// `None` while the action is in flight, `Some` once it finishes. CLI
    /// hooks write one record on completion so this is always populated.
    pub exit: Option<i32>,
}

impl ActivityRecord {
    /// Build a completed audit record: stamp the runtime metadata (timestamp,
    /// pid, ppid) and classify `cmd` for privilege escalation. One constructor
    /// for every surface — `helm exec`, `helm shell`, and `helm pane` — so the
    /// priv-escalation flag is derived identically no matter who logs.
    pub fn build(
        kind: ActivityKind,
        alias: &str,
        session: &str,
        cmd: &str,
        output_preview: &str,
        exit: Option<i32>,
    ) -> Self {
        ActivityRecord {
            ts_unix: now_unix(),
            pid: std::process::id(),
            ppid: ppid(),
            kind,
            alias: alias.to_string(),
            session: session.to_string(),
            cmd: cmd.to_string(),
            output_preview: output_preview.to_string(),
            has_privilege_escalation: has_privilege_escalation(cmd),
            exit,
        }
    }
}

/// Resolved helm state directory (`$XDG_STATE_HOME/helm` or the platform
/// fallback), created if missing. Shared by the activity log and the mosh
/// detection cache.
pub fn state_dir() -> Option<PathBuf> {
    let base = if let Some(v) = std::env::var_os("XDG_STATE_HOME") {
        PathBuf::from(v)
    } else if let Some(home) = std::env::var_os("HOME") {
        #[cfg(target_os = "macos")]
        {
            let mut p = PathBuf::from(home);
            p.push("Library");
            p.push("Application Support");
            p
        }
        #[cfg(not(target_os = "macos"))]
        {
            let mut p = PathBuf::from(home);
            p.push(".local");
            p.push("state");
            p
        }
    } else {
        return None;
    };
    let dir = base.join("helm");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("helm: state dir {}: {}", dir.display(), e);
        return None;
    }
    Some(dir)
}

/// Returns the resolved activity-log path, creating parent directories as
/// needed.
pub fn log_path() -> Option<PathBuf> {
    state_dir().map(|d| d.join("activity.jsonl"))
}

/// True for open failures that are *environmental* — the log lives on a
/// filesystem the caller can't write (helm invoked from inside a read-only
/// sandbox fence: EROFS, or a permission mask: EACCES). The log is documented
/// best-effort, and helm is one process per CLI call, so warning "once per
/// process" would still print once per call and pollute every captured
/// output an agent reads. These two kinds are silenced; every other error
/// (disk full, bad path, I/O) stays loud.
fn environmental_write_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ReadOnlyFilesystem | std::io::ErrorKind::PermissionDenied
    )
}

/// Append a record. Best-effort: a failure here never blocks the caller.
pub fn append(record: &ActivityRecord) {
    let Some(path) = log_path() else { return };
    append_to(&path, record);
}

/// [`append`]'s path-parameterized core, split out so tests can exercise the
/// write path against a temp file without the data-racy global `set_var`.
fn append_to(path: &std::path::Path, record: &ActivityRecord) {
    let line = match serde_json::to_string(record) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("helm: activity serialize failed: {e}");
            return;
        }
    };
    let mut file = match OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => f,
        Err(e) if environmental_write_error(&e) => return,
        Err(e) => {
            eprintln!("helm: activity open {}: {}", path.display(), e);
            return;
        }
    };
    if let Err(e) = writeln!(file, "{line}") {
        eprintln!("helm: activity write: {e}");
        return;
    }
    // Force the buffered line out to the OS so a subsequent SIGKILL /
    // panic / OOM-killer can't lose the audit entry. `flush()` on a File
    // is a no-op (no Rust-level buffering) but `sync_data()` issues an
    // fdatasync — the actual durability guarantee for an audit log.
    if let Err(e) = file.sync_data() {
        eprintln!("helm: activity sync: {e}");
    }
}

/// Read the tail of the log. Returns the last `limit` records (chronological
/// order — oldest first). Backs `helm activity`.
pub fn tail(limit: usize) -> Vec<ActivityRecord> {
    let Some(path) = log_path() else {
        return Vec::new();
    };
    tail_from(&path, limit)
}

/// [`tail`]'s path-parameterized core (see [`append_to`] for why).
fn tail_from(path: &std::path::Path, limit: usize) -> Vec<ActivityRecord> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<ActivityRecord> = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ActivityRecord>(line) {
            Ok(r) => out.push(r),
            Err(e) => {
                // Don't drop silently — surface to stderr so the
                // operator notices half-written / corrupted entries
                // that would otherwise vanish from the audit pane.
                eprintln!(
                    "helm: activity tail: skipping malformed line {} ({e})",
                    i + 1
                );
            }
        }
    }
    if out.len() > limit {
        let drop = out.len() - limit;
        out.drain(0..drop);
    }
    out
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Substring-and-word-boundary scan for `doas` or `sudo`. Conservative:
/// false positives are fine (the badge is informational), false negatives
/// are not. Matches at the start of the command, after pipes, after `&&`,
/// after `||`, after `;`, or after whitespace.
pub fn has_privilege_escalation(cmd: &str) -> bool {
    let mut prev_was_separator = true;
    let mut i = 0;
    let bytes = cmd.as_bytes();
    while i < bytes.len() {
        let c = bytes[i] as char;
        if prev_was_separator {
            let rest = &cmd[i..];
            for tok in &["doas ", "doas\t", "sudo ", "sudo\t"] {
                if rest.starts_with(tok) {
                    return true;
                }
            }
            if rest == "doas" || rest == "sudo" {
                return true;
            }
        }
        prev_was_separator = matches!(c, ' ' | '\t' | ';' | '|' | '&' | '\n' | '(');
        i += 1;
    }
    false
}

/// Extract first ≤2 non-empty lines from `s`, joined by `⏎` for single-row
/// rendering. Truncates each line to keep the preview short.
pub fn preview(s: &str) -> String {
    let mut picks: Vec<String> = Vec::new();
    for raw in s.lines() {
        let t = raw.trim_end();
        if t.is_empty() {
            continue;
        }
        let trimmed: String = t.chars().take(120).collect();
        picks.push(trimmed);
        if picks.len() == 2 {
            break;
        }
    }
    picks.join(" ⏎ ")
}

/// Best-effort getter for the current process's parent PID; used only as
/// metadata in the log.
pub fn ppid() -> u32 {
    // Fall back to 0 on platforms where it isn't reachable.
    #[cfg(unix)]
    {
        std::os::unix::process::parent_id()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environmental_write_error_silences_erofs_and_eacces_only() {
        assert!(environmental_write_error(
            &std::io::Error::from_raw_os_error(30)
        )); // EROFS
        assert!(environmental_write_error(
            &std::io::Error::from_raw_os_error(13)
        )); // EACCES
        assert!(!environmental_write_error(
            &std::io::Error::from_raw_os_error(28)
        )); // ENOSPC
        assert!(!environmental_write_error(
            &std::io::Error::from_raw_os_error(2)
        )); // ENOENT
    }

    #[test]
    fn append_tail_roundtrip_skips_malformed_and_caps() {
        let dir = std::env::temp_dir().join(format!("helm-act-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("activity.jsonl");
        let _ = std::fs::remove_file(&path);
        for i in 0..3 {
            let r = ActivityRecord::build(
                ActivityKind::Exec,
                "local",
                "helm-exec",
                &format!("echo {i}"),
                "out",
                Some(0),
            );
            append_to(&path, &r);
        }
        // A half-written line must be skipped, not abort the tail.
        use std::io::Write as _;
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{\"ts_unix\":1}}garbage").unwrap();
        let all = tail_from(&path, 10);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].cmd, "echo 0");
        let capped = tail_from(&path, 2);
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0].cmd, "echo 1"); // oldest dropped
        // Unwritable target (EACCES) is silent best-effort — must not panic.
        append_to(std::path::Path::new("/proc/1/root/x"), &all[0]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn privilege_escalation_at_start() {
        assert!(has_privilege_escalation("doas rcctl restart httpd"));
        assert!(has_privilege_escalation("sudo apt update"));
    }

    #[test]
    fn privilege_escalation_after_pipe() {
        assert!(has_privilege_escalation(
            "cat /etc/passwd | sudo tee /tmp/x"
        ));
    }

    #[test]
    fn privilege_escalation_after_and() {
        assert!(has_privilege_escalation("ls && doas rm /etc/foo"));
    }

    #[test]
    fn privilege_escalation_no_false_positive_on_substring() {
        assert!(!has_privilege_escalation("echo pseudoscience"));
        assert!(!has_privilege_escalation("undoasked"));
        assert!(!has_privilege_escalation("doasync"));
        assert!(!has_privilege_escalation("sudoers"));
    }

    #[test]
    fn privilege_escalation_across_newlines() {
        assert!(has_privilege_escalation("echo first\ndoas reboot"));
        assert!(has_privilege_escalation("ls\n\nsudo apt update"));
        assert!(!has_privilege_escalation("echo first\nls /tmp"));
    }

    #[test]
    fn preview_takes_first_two_nonempty_lines() {
        let p = preview("\n\nfirst\n\nsecond\nthird\n");
        assert_eq!(p, "first ⏎ second");
    }

    #[test]
    fn preview_handles_empty() {
        assert_eq!(preview(""), "");
    }

    #[test]
    fn build_flags_privilege_escalation_for_a_pane_run() {
        // A `helm pane run "doas …"` must carry the escalation flag exactly like
        // the remote shell does — the gap this constructor closed.
        let r = ActivityRecord::build(
            ActivityKind::ShellRun,
            "pane",
            "helm-logs",
            "doas rcctl restart httpd",
            "",
            Some(0),
        );
        assert_eq!(r.kind, ActivityKind::ShellRun);
        assert_eq!(r.alias, "pane");
        assert_eq!(r.session, "helm-logs");
        assert!(r.has_privilege_escalation);
        assert_eq!(r.exit, Some(0));
    }

    #[test]
    fn build_plain_command_carries_no_escalation_flag() {
        let r = ActivityRecord::build(
            ActivityKind::ShellRun,
            "pane",
            "helm",
            "cargo test",
            "",
            Some(1),
        );
        assert!(!r.has_privilege_escalation);
        assert_eq!(r.exit, Some(1));
    }
}
