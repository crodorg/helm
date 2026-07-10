//! Unit tests for `shell` (pure arg parsers + helpers). Split out to keep
//! shell.rs under the per-file line cap; included via `#[path]` so `super::*`
//! still reaches the module's private items.

use super::*;

fn v(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn run_args_basic_joins_command_words() {
    let r = parse_run_args(&v(&["web", "echo", "hi"])).unwrap();
    assert_eq!(r.target, "web");
    assert_eq!(r.cmd, "echo hi");
    assert_eq!(r.timeout, runcmd::DEFAULT_RUN_TIMEOUT_SECS);
}

#[test]
fn run_args_timeout_anywhere_after_target() {
    let r = parse_run_args(&v(&["web", "uptime", "--timeout", "5"])).unwrap();
    assert_eq!(r.cmd, "uptime");
    assert_eq!(r.timeout, 5);
}

#[test]
fn run_args_label_target_is_preserved() {
    let r = parse_run_args(&v(&["saas:deploy", "make", "deploy"])).unwrap();
    assert_eq!(r.target, "saas:deploy");
    assert_eq!(r.cmd, "make deploy");
}

#[test]
fn run_args_rejects_missing_target_and_empty_command() {
    assert!(parse_run_args(&v(&[])).is_err());
    assert!(parse_run_args(&v(&["web"])).is_err());
}

#[test]
fn run_args_rejects_bad_timeout() {
    assert!(parse_run_args(&v(&["web", "--timeout", "x", "uptime"])).is_err());
    assert!(parse_run_args(&v(&["web", "--timeout", "0", "uptime"])).is_err());
    assert!(parse_run_args(&v(&["web", "uptime", "--timeout"])).is_err());
}

#[test]
fn run_args_rejects_newline_command() {
    // A real argv can carry an embedded newline (e.g. a heredoc paste).
    let r = parse_run_args(&["web".to_string(), "echo a\necho b".to_string()]);
    assert!(r.is_err());
}

#[test]
fn label_from_target_extracts_after_colon() {
    assert_eq!(tmux_label_from_target("web"), "");
    assert_eq!(tmux_label_from_target("web:deploy"), "deploy");
    assert_eq!(tmux_label_from_target("local:claude"), "claude");
}

#[test]
fn read_args_defaults() {
    let r = parse_read_args(&v(&["web"])).unwrap();
    assert_eq!(r.target, "web");
    assert_eq!(r.lines, tmux::DEFAULT_CAPTURE_LINES);
    assert!(!r.raw);
    assert!(!r.delta);
}

#[test]
fn read_args_delta_flag_and_raw_conflict() {
    let r = parse_read_args(&v(&["web", "--delta"])).unwrap();
    assert!(r.delta);
    let r = parse_read_args(&v(&["web", "--delta", "-n", "50"])).unwrap();
    assert!(r.delta);
    assert_eq!(r.lines, 50);
    assert!(parse_read_args(&v(&["web", "--delta", "--raw"])).is_err());
}

#[test]
fn read_args_flags() {
    let r = parse_read_args(&v(&["web", "-n", "50", "--raw"])).unwrap();
    assert_eq!(r.lines, 50);
    assert!(r.raw);
}

#[test]
fn read_args_rejects_bad_and_missing() {
    assert!(parse_read_args(&v(&["web", "-n", "x"])).is_err());
    assert!(parse_read_args(&v(&["web", "-n"])).is_err());
    // -n 0 is rejected: `-S -0` is the whole pane, not zero lines, and the
    // message promises a positive integer.
    assert!(parse_read_args(&v(&["web", "-n", "0"])).is_err());
    assert!(parse_read_args(&v(&[])).is_err());
    // A second positional is ambiguous.
    assert!(parse_read_args(&v(&["web", "extra"])).is_err());
}

#[test]
fn shell_wait_rejects_bad_args_before_touching_tmux() {
    // All four bail in pure validation (usage, flag parse, label check)
    // before any tmux/ssh spawn; exercised for the exit-2 paths.
    let _ = shell_wait(&v(&[]));
    let _ = shell_wait(&v(&["web", "--timeout"]));
    let _ = shell_wait(&v(&["web", "--timeout", "0"]));
    let _ = shell_wait(&v(&["web:b@d"]));
}
