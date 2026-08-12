//! `helm shell run` / `helm pane run` — run a single command in a tmux
//! session or pane and capture *just its output and exit code*, via an
//! in-band sentinel the wrapper prints after the command. The poll loop runs
//! host-side (one ssh round-trip for a remote session; ssh-free for a local
//! pane), so the caller gets `output + exit: N` instead of a read→send→read
//! loop. Split from `tmux.rs` to keep both under the size cap: the thin tmux
//! verbs live there, the run engine here. Also home to the `wait` engine —
//! the sentinel-free sibling for interactive flows `run` refuses: block
//! host-side until the pane is back at a shell prompt (no exit code).

use anyhow::{Context, Result, anyhow};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::tmux::{LOCAL_ALIAS, parse_target, runner_cmd, shell_quote, tmux_prefix};

/// Default seconds `run` polls for its sentinel before returning the command
/// as still-running.
pub const DEFAULT_RUN_TIMEOUT_SECS: u32 = 30;

/// `case`-pattern of foreground commands that mean "the pane is idle at a
/// shell prompt" — the busy-guard keys off `#{pane_current_command}` rather
/// than matching the prompt string, which mis-classified themed prompts (fish
/// `❯`, `>`) as busy and REPL prompts (`mydb=#`) as idle. Anything else means
/// a command/pager/editor/REPL is running and `run` declines.
pub(crate) const IDLE_SHELLS: &str = "sh|bash|zsh|ksh|oksh|mksh|loksh|dash|ash|fish|tcsh|csh";

/// Monotonic per-process counter so concurrent `run` calls get distinct
/// sentinels without a wall clock. Paired with the pid it makes the sentinel
/// collision-proof in practice.
static RUN_SEQ: AtomicU32 = AtomicU32::new(0);

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

/// The line typed into the shell for a `run`: a start marker (`<tag>:s:`,
/// printed the moment the shell executes the line — the unambiguous "output
/// begins here" row, robust against the echoed input wrapping across pane
/// rows, which the old echo-line heuristic was not), the command, the exit
/// sentinel, then a `wait-for` signal on the per-call channel (plain `tmux`
/// — inside the pane `$TMUX` routes it to the right server).
fn run_payload(cmd: &str, tag: &str) -> String {
    format!("printf '{tag}:s:\\n'; {cmd}; printf '\\n{tag}:%d:\\n' \"$?\"; tmux wait-for -S {tag}")
}

/// Stateful one-shot: ensure the session, type `cmd` followed by a sentinel
/// `printf` and a `tmux wait-for -S` signal, then block on `tmux wait-for` —
/// *all in a single remote `sh` invocation*, so the wait happens on the host
/// rather than as repeated ssh round-trips. Completion is EVENT-driven: the
/// payload signals a per-call channel the runner is waiting on, waking it the
/// instant the command finishes (signals latch, so a command that finishes
/// before the runner reaches `wait-for` still wakes it — verified on tmux
/// 3.6b). A background watchdog (0.5s period) backstops the three cases the
/// event can't cover: shell death mid-command, timeout, and a pane whose
/// shell can't reach `tmux` (sentinel grep catches those). Returns the
/// command's output and exit code. `cmd` must be a single line (callers
/// reject embedded newlines, which would detach the sentinel from the
/// command). Non-interactive commands only; a pane sitting in a pager/editor
/// is reported as `busy`.
pub fn run_command(target: &str, cmd: &str, timeout_secs: u32) -> Result<RunOutcome> {
    let (alias, session) = parse_target(target);
    let q_session = shell_quote(&session);
    let tmux = tmux_prefix();
    let tag = run_tag();
    // See `run_payload`: start marker + command + exit sentinel + wait-for
    // signal. `;` chains so the sentinel printf always runs and `$?` reflects
    // the command's own exit; the printf runs BEFORE the signal, so the
    // sentinel is always captured once the runner wakes. The sentinel's
    // leading `\n` guarantees it starts a fresh line even if the command's
    // output lacks a trailing newline.
    let payload = run_payload(cmd, &tag);
    let q_payload = shell_quote(&payload);
    // Match the *result* sentinel (`<tag>:` then a digit), never the echoed
    // printf format (`<tag>:%d`), so the backstop can't exit early on the
    // input echo line.
    let q_grep = shell_quote(&format!("{tag}:[0-9]"));
    let q_busy = shell_quote(&format!("{tag}:busy:"));
    let q_gone = shell_quote(&format!("{tag}:gone:"));
    let q_chan = shell_quote(&tag);
    let iters = timeout_secs.max(1) * 2; // 0.5s watchdog period
    let script = build_run_script(
        &tmux, &q_session, &q_payload, &q_grep, &q_busy, &q_gone, &q_chan, iters,
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
/// session, busy-guard at a prompt, send the payload, then block on
/// `wait-for` (event-driven; a 0.5s watchdog backstops death/timeout/
/// unsignalable panes and wakes the waiter via the same channel), and emit
/// the final capture. Split out as a pure builder so the script's shape is
/// unit-tested without a tmux server. All `q_*` args are already
/// shell-quoted by the caller.
#[allow(clippy::too_many_arguments)]
fn build_run_script(
    tmux: &str,
    q_session: &str,
    q_payload: &str,
    q_grep: &str,
    q_busy: &str,
    q_gone: &str,
    q_chan: &str,
    iters: u32,
) -> String {
    format!(
        "{tmux} has-session -t {q_session} 2>/dev/null || \
           {tmux} new-session -d -x 200 -y 50 -s {q_session}; \
         sleep 0.3; \
         __c=$({tmux} display-message -p -t {q_session} '#{{pane_current_command}}'); __c=${{__c#-}}; \
         case \"$__c\" in {IDLE_SHELLS}) ;; *) printf '%s\\n' {q_busy}; exit 0 ;; esac; \
         {tmux} send-keys -t {q_session} -l -- {q_payload}; \
         {tmux} send-keys -t {q_session} Enter; \
         ( __i=0; while [ $__i -lt {iters} ]; do \
             {tmux} has-session -t {q_session} 2>/dev/null || break; \
             {tmux} capture-pane -t {q_session} -p -S -500 | grep -E -q -e {q_grep} && break; \
             sleep 0.5; __i=$(( __i + 1 )); \
           done; {tmux} wait-for -S {q_chan} ) >/dev/null 2>&1 & __w=$!; \
         {tmux} wait-for {q_chan}; \
         kill $__w 2>/dev/null; \
         {tmux} has-session -t {q_session} 2>/dev/null || {{ printf '%s\\n' {q_gone}; exit 0; }}; \
         {tmux} capture-pane -t {q_session} -p -S -500"
    )
}

/// The local `helm pane run` analogue of `build_run_script`. Targets a pane id
/// (`%N`) on the operator's own server — no ssh, no session create (the caller
/// resolved the pane). Guards on `#{pane_current_command}`, and uses
/// `#{pane_dead}` (not `has-session`) for death detection, since a pane whose
/// shell exits under `remain-on-exit` lingers as a dead pane. Same
/// event-driven wait-for + watchdog structure as `build_run_script`.
#[allow(clippy::too_many_arguments)]
fn build_pane_run_script(
    tmux: &str,
    q_pane: &str,
    q_payload: &str,
    q_grep: &str,
    q_busy: &str,
    q_gone: &str,
    q_chan: &str,
    iters: u32,
) -> String {
    format!(
        "__s=$({tmux} display-message -p -t {q_pane} '#{{pane_dead}}:#{{pane_current_command}}' 2>/dev/null) || \
           {{ printf '%s\\n' {q_gone}; exit 0; }}; \
         case \"$__s\" in 1:*) printf '%s\\n' {q_gone}; exit 0 ;; esac; \
         __c=${{__s#0:}}; __c=${{__c#-}}; \
         case \"$__c\" in {IDLE_SHELLS}) ;; *) printf '%s\\n' {q_busy}; exit 0 ;; esac; \
         {tmux} send-keys -t {q_pane} -l -- {q_payload}; \
         {tmux} send-keys -t {q_pane} Enter; \
         ( __i=0; while [ $__i -lt {iters} ]; do \
             __d=$({tmux} display-message -p -t {q_pane} '#{{pane_dead}}' 2>/dev/null) || break; \
             [ \"$__d\" = 1 ] && break; \
             {tmux} capture-pane -t {q_pane} -p -S -500 | grep -E -q -e {q_grep} && break; \
             sleep 0.5; __i=$(( __i + 1 )); \
           done; {tmux} wait-for -S {q_chan} ) >/dev/null 2>&1 & __w=$!; \
         {tmux} wait-for {q_chan}; \
         kill $__w 2>/dev/null; \
         __d=$({tmux} display-message -p -t {q_pane} '#{{pane_dead}}' 2>/dev/null); \
         case \"$__d\" in 1|'') printf '%s\\n' {q_gone}; exit 0 ;; esac; \
         {tmux} capture-pane -t {q_pane} -p -S -500"
    )
}

/// Run `cmd` in an already-resolved window pane (`pane_id`, e.g. `%3`) on the
/// local tmux server, returning its output and exit code. The `helm pane run`
/// engine — same sentinel + wait-for scheme as `run_command`, but
/// pane-targeted and ssh-free.
pub fn run_in_pane(pane_id: &str, cmd: &str, timeout_secs: u32) -> Result<RunOutcome> {
    let q_pane = shell_quote(pane_id);
    let tmux = tmux_prefix();
    let tag = run_tag();
    let payload = run_payload(cmd, &tag);
    let q_payload = shell_quote(&payload);
    let q_grep = shell_quote(&format!("{tag}:[0-9]"));
    let q_busy = shell_quote(&format!("{tag}:busy:"));
    let q_gone = shell_quote(&format!("{tag}:gone:"));
    let q_chan = shell_quote(&tag);
    let iters = timeout_secs.max(1) * 2; // 0.5s watchdog period
    let script = build_pane_run_script(
        &tmux, &q_pane, &q_payload, &q_grep, &q_busy, &q_gone, &q_chan, iters,
    );
    let out = runner_cmd(LOCAL_ALIAS, &script)
        .output()
        .context("spawn pane run runner")?;
    if !out.status.success() {
        return Err(anyhow!(
            "`helm pane run` failed (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(extract_run(&String::from_utf8_lossy(&out.stdout), &tag))
}

/// The `helm exec local` engine: run `cmd` in an *ephemeral* session on the
/// local tmux server — created for this call in the caller's cwd, killed
/// after the capture — so one shot ≈ one fresh shell, like the plain fork
/// this replaced (no sticky `cd`/`export`, no busy collisions between
/// concurrent execs, no command/output lingering in a well-known session's
/// scrollback). Routing through the server is the point: helm may be invoked
/// from inside a sandboxed caller, and the server-side fork escapes that
/// sandbox. Returns the session's label alongside the outcome — on timeout
/// the session is left alive (killing it would SIGHUP the still-running
/// command), and the label lets the caller point at `helm shell read
/// local:<label>`. `busy` is never set (the session is private to this call).
pub fn run_ephemeral_local(
    cmd: &str,
    timeout_secs: u32,
    cwd: &std::path::Path,
) -> Result<(RunOutcome, String)> {
    let tmux = tmux_prefix();
    let tag = run_tag();
    let (label, session) = exec_label(&tag);
    let q_session = shell_quote(&session);
    let q_cwd = shell_quote(&cwd.to_string_lossy());
    let payload = run_payload(cmd, &tag);
    let q_payload = shell_quote(&payload);
    let q_grep = shell_quote(&format!("{tag}:[0-9]"));
    let q_gone = shell_quote(&format!("{tag}:gone:"));
    let q_chan = shell_quote(&tag);
    let iters = timeout_secs.max(1) * 2; // 0.5s watchdog period
    let script = build_ephemeral_run_script(
        &tmux, &q_session, &q_cwd, &q_payload, &q_grep, &q_gone, &q_chan, iters,
    );
    let out = runner_cmd(LOCAL_ALIAS, &script)
        .output()
        .context("spawn exec local runner")?;
    if !out.status.success() {
        return Err(anyhow!(
            "`helm exec local` failed (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok((
        extract_run(&String::from_utf8_lossy(&out.stdout), &tag),
        label,
    ))
}

/// Session label + name for one ephemeral exec-local call. The per-call tag
/// is the uniqueness source; the `exec-` prefix keeps the session apart from
/// operator `local:*` sessions and legible in `tmux ls`
/// (`helm-exec-<pid>_<seq>`). Pure so the valid-label invariant is
/// unit-tested: the timeout narration hands the label to `helm shell
/// read/close local:<label>`, which rejects labels outside `valid_label`.
fn exec_label(tag: &str) -> (String, String) {
    let label = format!("exec-{}", tag.trim_start_matches("__helm_"));
    let session = format!("helm-{label}");
    (label, session)
}

/// The ephemeral-session analogue of `build_run_script`. Creates the per-call
/// session in the caller's cwd (`-c`; tmux falls back to its default when the
/// dir doesn't exist server-side), so no busy-guard: the shell is born idle
/// and private. Same event-driven wait-for + watchdog as `build_run_script`.
/// The final capture is held in a variable so the session can be killed
/// after a *completed* run (sentinel present) but left alive on timeout —
/// killing it then would SIGHUP the still-running command.
#[allow(clippy::too_many_arguments)]
fn build_ephemeral_run_script(
    tmux: &str,
    q_session: &str,
    q_cwd: &str,
    q_payload: &str,
    q_grep: &str,
    q_gone: &str,
    q_chan: &str,
    iters: u32,
) -> String {
    format!(
        "{tmux} new-session -d -x 200 -y 50 -c {q_cwd} -s {q_session} || exit 97; \
         {tmux} send-keys -t {q_session} -l -- {q_payload}; \
         {tmux} send-keys -t {q_session} Enter; \
         ( __i=0; while [ $__i -lt {iters} ]; do \
             {tmux} has-session -t {q_session} 2>/dev/null || break; \
             {tmux} capture-pane -t {q_session} -p -S -500 | grep -E -q -e {q_grep} && break; \
             sleep 0.5; __i=$(( __i + 1 )); \
           done; {tmux} wait-for -S {q_chan} ) >/dev/null 2>&1 & __w=$!; \
         {tmux} wait-for {q_chan}; \
         kill $__w 2>/dev/null; \
         {tmux} has-session -t {q_session} 2>/dev/null || {{ printf '%s\\n' {q_gone}; exit 0; }}; \
         __cap=$({tmux} capture-pane -t {q_session} -p -S -500); \
         printf '%s\\n' \"$__cap\"; \
         printf '%s\\n' \"$__cap\" | grep -E -q -e {q_grep} && \
           {tmux} kill-session -t {q_session} 2>/dev/null; \
         exit 0"
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
    // Output begins after the start-marker row the wrapper printf'd — the
    // LAST exact `<tag>:s:` line (rposition skips any forged copy a wrapped
    // input echo might leave). Fall back to the echoed printf format for
    // captures without a marker (shell died before the wrapper ran).
    let start_marker = format!("{tag}:s:");
    let echo_needle = format!("{tag}:%d"); // the typed printf format
    let start_after = |region: &[&str]| -> usize {
        region
            .iter()
            .rposition(|l| l.trim_end() == start_marker)
            .or_else(|| region.iter().rposition(|l| l.contains(&echo_needle)))
            .map(|i| i + 1)
            .unwrap_or(0)
    };
    match lines.iter().rposition(|l| sentinel_exit(l, tag).is_some()) {
        Some(s_idx) => {
            let exit = sentinel_exit(lines[s_idx], tag);
            let start = start_after(&lines[..s_idx]);
            RunOutcome {
                output: strip_trailing_blank(&lines[start..s_idx].join("\n")),
                exit,
                busy: false,
                gone: false,
            }
        }
        None => {
            // No sentinel — command still running at timeout. Everything
            // after the start marker (or the echoed input line).
            let start = start_after(&lines);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ephemeral_script_creates_in_cwd_and_kills_only_after_sentinel() {
        let s = build_ephemeral_run_script(
            "tmux",
            "'helm-exec-1-0'",
            "'/proj'",
            "'payload'",
            "'__helm_1_0:[0-9]'",
            "'__helm_1_0:gone:'",
            "'__helm_1_0'",
            60,
        );
        // Fresh session in the caller's cwd; hard-fails (exit 97) if the
        // server can't create it — never a busy-guard, never a reuse.
        assert!(s.contains("new-session -d -x 200 -y 50 -c '/proj' -s 'helm-exec-1-0' || exit 97"));
        // The kill is gated on the sentinel being present in the capture: a
        // timed-out (still running) command must keep its session alive.
        let kill_at = s.find("kill-session").expect("kill-session present");
        // rfind: the same grep pattern also appears in the watchdog loop;
        // the *last* occurrence is the kill gate.
        let gate_at = s
            .rfind("grep -E -q -e '__helm_1_0:[0-9]' && ")
            .expect("gated");
        assert!(gate_at < kill_at);
        // No busy sentinel — the session is private to the call.
        assert!(!s.contains(":busy:"));
    }

    #[test]
    fn ephemeral_sessions_are_unique_per_call() {
        // The label derives from the per-process tag counter, so two calls
        // can't collide on a session (the source of the old busy/sticky bugs).
        let a = run_tag();
        let b = run_tag();
        assert_ne!(a, b);
        let (label_a, session_a) = exec_label(&a);
        let (label_b, _) = exec_label(&b);
        assert_ne!(label_a, label_b);
        assert_eq!(session_a, format!("helm-{label_a}"));
        assert!(label_a.starts_with("exec-"));
        // The timeout narration points `helm shell read/close local:<label>`
        // at this label — it must pass the target label validator.
        assert!(crate::tmux::valid_label(&label_a), "{label_a}");
    }

    #[test]
    fn run_payload_wraps_cmd_with_markers_and_signal() {
        let p = run_payload("echo hi", "__helm_9_9");
        // Start marker first, command, exit sentinel with the shell's $?,
        // then the wait-for wake — in that order.
        let s = p.find("__helm_9_9:s:").expect("start marker");
        let c = p.find("echo hi").expect("command");
        let e = p.find("__helm_9_9:%d:").expect("exit sentinel");
        let w = p.find("tmux wait-for -S __helm_9_9").expect("signal");
        assert!(s < c && c < e && e < w, "{p}");
        assert!(p.contains("\"$?\""), "{p}");
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
            "'t'",
            60,
        );
        assert!(s.contains("has-session -t helm"));
        assert!(s.contains("new-session -d -x 200 -y 50 -s helm"));
        assert!(s.contains("send-keys -t helm -l -- 'payload'"));
        assert!(s.contains("send-keys -t helm Enter"));
        // Busy-guard keys off the foreground command, not the prompt string.
        assert!(s.contains("display-message -p -t helm '#{pane_current_command}'"));
        assert!(s.contains("sh|bash|zsh|ksh|oksh"));
        // Event-driven wait: foreground blocks on the per-call channel; the
        // backstop watchdog greps for the sentinel and signals the same
        // channel on death/timeout.
        assert!(s.contains("tmux -u wait-for 't'"));
        assert!(s.contains("wait-for -S 't'"));
        assert!(s.contains("kill $__w"));
        assert!(s.contains("grep -E -q -e 't:[0-9]'"));
        assert!(s.contains("printf '%s\\n' 't:busy:'"));
        assert!(s.contains("printf '%s\\n' 't:gone:'"));
        assert!(s.contains("[ $__i -lt 60 ]"));
        assert!(s.contains("sleep 0.5"));
        assert!(s.contains("capture-pane -t helm -p -S -500"));
    }

    #[test]
    fn build_pane_run_script_targets_a_pane_and_checks_death() {
        let s = build_pane_run_script(
            "tmux",
            "%3",
            "'payload'",
            "'t:[0-9]'",
            "'t:busy:'",
            "'t:gone:'",
            "'t'",
            20,
        );
        // Targets the pane id, not a session, and never creates one.
        assert!(s.contains("send-keys -t %3 -l -- 'payload'"));
        assert!(!s.contains("new-session"));
        assert!(!s.contains("has-session"));
        // Busy-guard via current command; death via pane_dead.
        assert!(s.contains("#{pane_dead}:#{pane_current_command}"));
        assert!(s.contains("sh|bash|zsh|ksh|oksh"));
        assert!(s.contains("#{pane_dead}"));
        // Same event-driven wait + watchdog as the session script.
        assert!(s.contains("tmux wait-for 't'"));
        assert!(s.contains("wait-for -S 't'"));
        assert!(s.contains("[ $__i -lt 20 ]"));
        assert!(s.contains("printf '%s\\n' 't:gone:'"));
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
    fn extract_run_starts_at_the_start_marker() {
        // The modern wrapper prints `<tag>:s:` when the line executes; output
        // is everything between it and the exit sentinel, so a wrapped input
        // echo (which splits across pane rows and defeats the echo-needle
        // heuristic) can't leak into the output.
        let tag = "__helm_1_0";
        let cap = "$ printf '__helm_1_0:s:\\n'; echo hi; printf '\\n__helm_1_0:%d:\\n' \"$?\"; tmux wa\n\
                   it-for -S __helm_1_0\n\
                   __helm_1_0:s:\nhi\n\n__helm_1_0:0:\n$\n";
        let out = extract_run(cap, tag);
        assert_eq!(out.output, "hi");
        assert_eq!(out.exit, Some(0));
    }

    #[test]
    fn extract_run_timeout_starts_at_the_start_marker() {
        let tag = "__helm_1_0";
        let cap = "$ echo-echo __helm_1_0:%d: fragment\n__helm_1_0:s:\nstill working\n";
        let out = extract_run(cap, tag);
        assert_eq!(out.exit, None);
        assert_eq!(out.output, "still working");
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
