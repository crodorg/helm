//! `helm shell watch` / `helm pane watch` — block until a predicate holds on a
//! tmux session or pane, then park: the poll loop runs host-side in ONE runner
//! invocation (one ssh round-trip for a remote alias, ssh-free for a local
//! pane), so the caller blocks on a single call and gets back a settled
//! session/pane to drive with `send` + `read --delta`. This is the generalized
//! sibling of `run`: `run` wraps a command and returns its exit code; `watch`
//! wraps nothing and returns only *whether the predicate held*.
//!
//! Predicates (exactly one per call — more combinators may come later):
//!   `--idle`         the pane is back at a shell prompt (the classic `wait`;
//!                    keyed off `#{pane_current_command}` vs `IDLE_SHELLS`,
//!                    same test the `run` busy-guard uses)
//!   `--match REGEX`  a line matching the extended regex (`grep -E`) appears in
//!                    the pane's *new* output since the watch started
//!
//! `helm shell wait` / `helm pane wait` are preserved as-is: they call this
//! engine with `Predicate::Idle`.

use anyhow::{Context, Result, anyhow};

use crate::runcmd::IDLE_SHELLS;
use crate::tmux::{LOCAL_ALIAS, parse_target, runner_cmd, shell_quote, tmux_prefix};

/// Default seconds a `watch`/`wait` blocks before reporting the predicate as
/// unmet (`timeout`). Longer than `run`'s: watch exists for interactive/long
/// flows (doas prompts, deploys) where the agent parked the pane with `send`.
pub const DEFAULT_WATCH_TIMEOUT_SECS: u32 = 60;

/// Markers the watch scripts print on their own stdout (never into the pane),
/// so parsing can't collide with pane content.
const WATCH_DONE: &str = "__helm_watch:done:";
const WATCH_TIMEOUT: &str = "__helm_watch:timeout:";
const WATCH_GONE: &str = "__helm_watch:gone:";

/// The condition a `watch` blocks on. Exactly one per invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Predicate {
    /// The pane's foreground command is a known shell — it is back at a prompt.
    Idle,
    /// A line matching this extended regex appears in output produced after
    /// the watch began.
    Match(String),
}

/// Outcome of a `helm shell watch` / `helm pane watch`: did the predicate
/// hold? No exit code — nothing was wrapped (that's `run`'s job); `watch`
/// pairs with `send` + `read --delta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchOutcome {
    /// The predicate held (idle reached / pattern matched).
    Done,
    /// The predicate never held before the poll timed out.
    Timeout,
    /// The session/pane is gone (never existed, or died while watching).
    Gone,
}

impl WatchOutcome {
    /// Activity-log fields for this outcome: the outcome tag and the exit
    /// column (mirrors `run`'s convention: 0 done, none while running, 1 gone).
    pub fn logged(self) -> (&'static str, Option<i32>) {
        match self {
            WatchOutcome::Done => ("done", Some(0)),
            WatchOutcome::Timeout => ("timeout", None),
            WatchOutcome::Gone => ("gone", Some(1)),
        }
    }

    /// The process exit byte, matching the `run` contract
    /// (`opts::run_exit_byte`): 0 done, 124 timeout, 1 gone.
    pub fn exit_byte(self) -> u8 {
        match self {
            WatchOutcome::Done => crate::opts::run_exit_byte(false, false, Some(0)),
            WatchOutcome::Timeout => crate::opts::run_exit_byte(false, false, None),
            WatchOutcome::Gone => crate::opts::run_exit_byte(false, true, None),
        }
    }

    /// Human report line (without the `helm: ` prefix). `pred` tailors the
    /// wording (idle vs match); `name` names the session/pane; `read_hint`/
    /// `reopen_hint` are the surface-specific follow-up commands.
    pub fn report(
        self,
        pred: &Predicate,
        name: &str,
        timeout_secs: u32,
        read_hint: &str,
        reopen_hint: &str,
    ) -> String {
        match self {
            WatchOutcome::Done => match pred {
                Predicate::Idle => {
                    format!("{name} is at a shell prompt — new output: `{read_hint}`")
                }
                Predicate::Match(_) => {
                    format!("{name} matched — new output: `{read_hint}`")
                }
            },
            WatchOutcome::Timeout => match pred {
                Predicate::Idle => format!(
                    "still busy after {timeout_secs}s (timeout). Wait again, or peek with `{read_hint}`."
                ),
                Predicate::Match(_) => format!(
                    "no match after {timeout_secs}s (timeout). Watch again, or peek with `{read_hint}`."
                ),
            },
            WatchOutcome::Gone => format!("{name} is gone. Reopen with `{reopen_hint}`."),
        }
    }
}

/// Parse the flag tail shared by `helm shell watch` and `helm pane watch`:
/// an optional predicate (`--idle` | `--match REGEX`, default `--idle`) and an
/// optional `--timeout SECS`, in any order. Exactly one predicate. `Err` is the
/// bare reason; callers prepend their command prefix / usage line.
pub fn parse_args(args: &[String], default_secs: u32) -> Result<(Predicate, u32), &'static str> {
    let mut pred: Option<Predicate> = None;
    let mut timeout = default_secs;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--idle" => {
                if pred.is_some() {
                    return Err("choose one predicate: --idle or --match");
                }
                pred = Some(Predicate::Idle);
                i += 1;
            }
            "--match" => {
                if pred.is_some() {
                    return Err("choose one predicate: --idle or --match");
                }
                let pat = args
                    .get(i + 1)
                    .ok_or("--match requires a pattern (regex)")?;
                if pat.is_empty() {
                    return Err("--match pattern must not be empty");
                }
                pred = Some(Predicate::Match(pat.clone()));
                i += 2;
            }
            "--timeout" => {
                let val = args
                    .get(i + 1)
                    .ok_or("--timeout requires a value (seconds)")?;
                timeout = crate::opts::parse_timeout(val)?;
                i += 2;
            }
            _ => return Err("unexpected argument"),
        }
    }
    Ok((pred.unwrap_or(Predicate::Idle), timeout))
}

/// Host-side poll script for the idle predicate on a *session*: block until the
/// session's active pane is idle at a shell prompt (same `#{pane_current_command}`
/// / `IDLE_SHELLS` test as the run busy-guard). Never creates the session —
/// watching a session that doesn't exist reports `gone`, since a fresh shell
/// would be trivially "done". Requires TWO consecutive idle samples (0.2s
/// apart, after a 0.3s settle) so a `send` → `watch` race — polling before the
/// sent command has spawned — can't declare done early.
fn build_idle_session_script(tmux: &str, q_session: &str, iters: u32) -> String {
    format!(
        "{tmux} has-session -t {q_session} 2>/dev/null || {{ printf '%s\\n' '{WATCH_GONE}'; exit 0; }}; \
         sleep 0.3; \
         __ok=0; __i=0; while [ $__i -lt {iters} ]; do \
           __c=$({tmux} display-message -p -t {q_session} '#{{pane_current_command}}' 2>/dev/null) || \
             {{ printf '%s\\n' '{WATCH_GONE}'; exit 0; }}; \
           __c=${{__c#-}}; \
           case \"$__c\" in {IDLE_SHELLS}) __ok=$(( __ok + 1 )); \
             [ $__ok -ge 2 ] && {{ printf '%s\\n' '{WATCH_DONE}'; exit 0; }} ;; *) __ok=0 ;; esac; \
           sleep 0.2; __i=$(( __i + 1 )); \
         done; \
         printf '%s\\n' '{WATCH_TIMEOUT}'"
    )
}

/// The local *pane* analogue of `build_idle_session_script`. Targets a pane id
/// and uses `#{pane_dead}` for death detection, like the pane run script.
fn build_idle_pane_script(tmux: &str, q_pane: &str, iters: u32) -> String {
    format!(
        "sleep 0.3; \
         __ok=0; __i=0; while [ $__i -lt {iters} ]; do \
           __s=$({tmux} display-message -p -t {q_pane} '#{{pane_dead}}:#{{pane_current_command}}' 2>/dev/null) || \
             {{ printf '%s\\n' '{WATCH_GONE}'; exit 0; }}; \
           case \"$__s\" in 1:*) printf '%s\\n' '{WATCH_GONE}'; exit 0 ;; esac; \
           __c=${{__s#0:}}; __c=${{__c#-}}; \
           case \"$__c\" in {IDLE_SHELLS}) __ok=$(( __ok + 1 )); \
             [ $__ok -ge 2 ] && {{ printf '%s\\n' '{WATCH_DONE}'; exit 0; }} ;; *) __ok=0 ;; esac; \
           sleep 0.2; __i=$(( __i + 1 )); \
         done; \
         printf '%s\\n' '{WATCH_TIMEOUT}'"
    )
}

/// Host-side poll script for the match predicate on a *session*: block until a
/// line matching `q_pat` (a shell-quoted extended regex) appears in output the
/// session produces *after the watch starts*. `__base` snapshots the absolute
/// index of the last content line at start (`#{history_size} + #{cursor_y}`);
/// each poll captures the full scrollback, `awk` drops the blank padding
/// `capture-pane` adds and slices only the lines past `__base`, then greps
/// those. Keying off the cursor — not a raw line count — is what makes it
/// correct: on-screen output never grows the padded line count, and the
/// echoed command line (which sits at/above the start cursor) is excluded, so
/// a pattern that also appears in the typed command can't false-trigger. A
/// 0.3s settle before the baseline lets a just-`send`-ed command echo and
/// advance the cursor first, closing the send→watch race (same guard the idle
/// script uses) — without it the baseline can predate the echo and match it.
fn build_match_session_script(tmux: &str, q_session: &str, q_pat: &str, iters: u32) -> String {
    format!(
        "{tmux} has-session -t {q_session} 2>/dev/null || {{ printf '%s\\n' '{WATCH_GONE}'; exit 0; }}; \
         sleep 0.3; \
         __base=$({tmux} display-message -p -t {q_session} '#{{history_size}} #{{cursor_y}}' 2>/dev/null | awk '{{print $1 + $2}}'); \
         __i=0; while [ $__i -lt {iters} ]; do \
           {tmux} has-session -t {q_session} 2>/dev/null || {{ printf '%s\\n' '{WATCH_GONE}'; exit 0; }}; \
           {tmux} capture-pane -t {q_session} -p -S - 2>/dev/null \
             | awk -v b=\"$__base\" '{{a[NR]=$0}} END{{last=NR; while (last>0 && a[last] ~ /^[ \\t]*$/) last--; for (i=b+1; i<=last; i++) print a[i]}}' \
             | grep -E -q -- {q_pat} && {{ printf '%s\\n' '{WATCH_DONE}'; exit 0; }}; \
           sleep 0.2; __i=$(( __i + 1 )); \
         done; \
         printf '%s\\n' '{WATCH_TIMEOUT}'"
    )
}

/// The local *pane* analogue of `build_match_session_script`. Targets a pane id
/// and uses `#{pane_dead}` for death detection.
fn build_match_pane_script(tmux: &str, q_pane: &str, q_pat: &str, iters: u32) -> String {
    format!(
        "__d=$({tmux} display-message -p -t {q_pane} '#{{pane_dead}}' 2>/dev/null); \
         case \"$__d\" in 1|'') printf '%s\\n' '{WATCH_GONE}'; exit 0 ;; esac; \
         sleep 0.3; \
         __base=$({tmux} display-message -p -t {q_pane} '#{{history_size}} #{{cursor_y}}' 2>/dev/null | awk '{{print $1 + $2}}'); \
         __i=0; while [ $__i -lt {iters} ]; do \
           __d=$({tmux} display-message -p -t {q_pane} '#{{pane_dead}}' 2>/dev/null); \
           case \"$__d\" in 1|'') printf '%s\\n' '{WATCH_GONE}'; exit 0 ;; esac; \
           {tmux} capture-pane -t {q_pane} -p -S - 2>/dev/null \
             | awk -v b=\"$__base\" '{{a[NR]=$0}} END{{last=NR; while (last>0 && a[last] ~ /^[ \\t]*$/) last--; for (i=b+1; i<=last; i++) print a[i]}}' \
             | grep -E -q -- {q_pat} && {{ printf '%s\\n' '{WATCH_DONE}'; exit 0; }}; \
           sleep 0.2; __i=$(( __i + 1 )); \
         done; \
         printf '%s\\n' '{WATCH_TIMEOUT}'"
    )
}

/// Pure parser for the watch scripts' stdout: exactly one marker line.
fn parse_watch(out: &str) -> Result<WatchOutcome> {
    for line in out.lines() {
        match line.trim_end() {
            WATCH_DONE => return Ok(WatchOutcome::Done),
            WATCH_TIMEOUT => return Ok(WatchOutcome::Timeout),
            WATCH_GONE => return Ok(WatchOutcome::Gone),
            _ => {}
        }
    }
    Err(anyhow!("watch: unrecognized runner output: {}", out.trim()))
}

/// Number of poll iterations for a given block timeout: one every 0.2s.
fn iters_for(timeout_secs: u32) -> u32 {
    timeout_secs.max(1) * 5
}

/// Block until `pred` holds on a session — the poll loop runs host-side in ONE
/// runner invocation (one ssh round-trip for a remote alias), like `run`.
pub fn watch_session(target: &str, pred: &Predicate, timeout_secs: u32) -> Result<WatchOutcome> {
    let (alias, session) = parse_target(target);
    let q_session = shell_quote(&session);
    let tmux = tmux_prefix();
    let iters = iters_for(timeout_secs);
    let script = match pred {
        Predicate::Idle => build_idle_session_script(&tmux, &q_session, iters),
        Predicate::Match(p) => {
            build_match_session_script(&tmux, &q_session, &shell_quote(p), iters)
        }
    };
    let out = runner_cmd(&alias, &script)
        .output()
        .context("spawn tmux watch runner")?;
    if !out.status.success() {
        return Err(anyhow!(
            "`helm shell watch` on {alias} failed (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_watch(&String::from_utf8_lossy(&out.stdout))
}

/// Block until `pred` holds on an already-resolved local window pane (`%N`).
/// The `helm pane watch` engine — ssh-free, `#{pane_dead}` for death.
pub fn watch_pane(pane_id: &str, pred: &Predicate, timeout_secs: u32) -> Result<WatchOutcome> {
    let q_pane = shell_quote(pane_id);
    let tmux = tmux_prefix();
    let iters = iters_for(timeout_secs);
    let script = match pred {
        Predicate::Idle => build_idle_pane_script(&tmux, &q_pane, iters),
        Predicate::Match(p) => build_match_pane_script(&tmux, &q_pane, &shell_quote(p), iters),
    };
    let out = runner_cmd(LOCAL_ALIAS, &script)
        .output()
        .context("spawn pane watch runner")?;
    if !out.status.success() {
        return Err(anyhow!(
            "`helm pane watch` failed (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_watch(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_args_defaults_to_idle() {
        assert_eq!(parse_args(&v(&[]), 60), Ok((Predicate::Idle, 60)));
        assert_eq!(parse_args(&v(&["--idle"]), 60), Ok((Predicate::Idle, 60)));
        assert_eq!(
            parse_args(&v(&["--timeout", "5"]), 60),
            Ok((Predicate::Idle, 5))
        );
    }

    #[test]
    fn parse_args_reads_match_pattern_and_timeout_any_order() {
        assert_eq!(
            parse_args(&v(&["--match", "done$"]), 60),
            Ok((Predicate::Match("done$".into()), 60))
        );
        assert_eq!(
            parse_args(&v(&["--timeout", "10", "--match", "ok"]), 60),
            Ok((Predicate::Match("ok".into()), 10))
        );
        assert_eq!(
            parse_args(&v(&["--match", "ok", "--timeout", "10"]), 60),
            Ok((Predicate::Match("ok".into()), 10))
        );
    }

    #[test]
    fn parse_args_rejects_bad_input() {
        // Two predicates.
        assert!(parse_args(&v(&["--idle", "--match", "x"]), 60).is_err());
        assert!(parse_args(&v(&["--match", "a", "--match", "b"]), 60).is_err());
        // Missing / empty pattern.
        assert!(parse_args(&v(&["--match"]), 60).is_err());
        assert!(parse_args(&v(&["--match", ""]), 60).is_err());
        // Bad timeout.
        assert!(parse_args(&v(&["--timeout"]), 60).is_err());
        assert!(parse_args(&v(&["--timeout", "0"]), 60).is_err());
        // Stray token.
        assert!(parse_args(&v(&["extra"]), 60).is_err());
    }

    #[test]
    fn idle_session_script_polls_and_never_creates() {
        let s = build_idle_session_script("tmux -u", "helm-deploy", 300);
        assert!(s.contains("has-session -t helm-deploy"));
        assert!(!s.contains("new-session"));
        assert!(!s.contains("send-keys"));
        assert!(s.contains("display-message -p -t helm-deploy '#{pane_current_command}'"));
        assert!(s.contains("sh|bash|zsh|ksh|oksh"));
        assert!(s.contains("[ $__ok -ge 2 ]"));
        assert!(s.contains("[ $__i -lt 300 ]"));
        assert!(s.contains(WATCH_DONE));
        assert!(s.contains(WATCH_TIMEOUT));
        assert!(s.contains(WATCH_GONE));
    }

    #[test]
    fn idle_pane_script_targets_a_pane_and_checks_death() {
        let s = build_idle_pane_script("tmux", "%3", 50);
        assert!(s.contains("-t %3"));
        assert!(!s.contains("has-session"));
        assert!(!s.contains("new-session"));
        assert!(s.contains("#{pane_dead}:#{pane_current_command}"));
        assert!(s.contains("[ $__ok -ge 2 ]"));
        assert!(s.contains(WATCH_DONE));
    }

    #[test]
    fn match_session_script_greps_only_new_output() {
        let s = build_match_session_script("tmux", "helm-web", "'done$'", 300);
        assert!(s.contains("has-session -t helm-web"));
        assert!(!s.contains("new-session"));
        // Cursor/history baseline + awk slice so padding and old text (incl.
        // the echoed command) can't match.
        assert!(s.contains("__base="));
        assert!(s.contains("#{history_size} #{cursor_y}"));
        assert!(s.contains("awk -v b="));
        assert!(s.contains("grep -E -q -- 'done$'"));
        assert!(s.contains("[ $__i -lt 300 ]"));
        assert!(s.contains(WATCH_DONE));
        assert!(s.contains(WATCH_TIMEOUT));
    }

    #[test]
    fn match_pane_script_checks_death_and_greps_new_output() {
        let s = build_match_pane_script("tmux", "%7", "'ERROR'", 50);
        assert!(s.contains("-t %7"));
        assert!(s.contains("#{pane_dead}"));
        assert!(s.contains("#{history_size} #{cursor_y}"));
        assert!(s.contains("awk -v b="));
        assert!(s.contains("grep -E -q -- 'ERROR'"));
        assert!(s.contains(WATCH_GONE));
    }

    #[test]
    fn watch_outcome_log_exit_and_report_per_state() {
        assert_eq!(WatchOutcome::Done.logged(), ("done", Some(0)));
        assert_eq!(WatchOutcome::Timeout.logged(), ("timeout", None));
        assert_eq!(WatchOutcome::Gone.logged(), ("gone", Some(1)));
        assert_eq!(WatchOutcome::Done.exit_byte(), 0);
        assert_eq!(WatchOutcome::Timeout.exit_byte(), 124);
        assert_eq!(WatchOutcome::Gone.exit_byte(), 1);
        // Idle wording (the classic `wait` text — must not drift).
        let r = WatchOutcome::Done.report(
            &Predicate::Idle,
            "session web",
            60,
            "helm shell read web --delta",
            "x",
        );
        assert!(r.contains("shell prompt"));
        assert!(r.contains("helm shell read web --delta"));
        let r = WatchOutcome::Timeout.report(&Predicate::Idle, "session web", 5, "read", "x");
        assert!(r.contains("5s"));
        assert!(r.contains("still busy"));
        // Match wording.
        let m = Predicate::Match("done$".into());
        let r = WatchOutcome::Done.report(&m, "session web", 60, "read", "x");
        assert!(r.contains("matched"));
        let r = WatchOutcome::Timeout.report(&m, "session web", 7, "read", "x");
        assert!(r.contains("no match"));
        assert!(r.contains("7s"));
        let r = WatchOutcome::Gone.report(&m, "session web", 60, "read", "helm shell open -d web");
        assert!(r.contains("gone"));
        assert!(r.contains("helm shell open -d web"));
    }

    #[test]
    fn parse_watch_maps_each_marker() {
        assert_eq!(
            parse_watch("__helm_watch:done:\n").unwrap(),
            WatchOutcome::Done
        );
        assert_eq!(
            parse_watch("__helm_watch:timeout:\n").unwrap(),
            WatchOutcome::Timeout
        );
        assert_eq!(
            parse_watch("__helm_watch:gone:\n").unwrap(),
            WatchOutcome::Gone
        );
        // Noise around the marker (e.g. a PATH export warning) is tolerated.
        assert_eq!(
            parse_watch("warning: x\n__helm_watch:done:\n").unwrap(),
            WatchOutcome::Done
        );
        assert!(parse_watch("").is_err());
        assert!(parse_watch("garbage\n").is_err());
    }
}
