//! Unit tests for `pane` (pure helpers). Split out to keep pane.rs under
//! the per-file line cap; included via `#[path]` so `super::*` still reaches
//! the module's private items.

use super::*;

#[test]
fn parse_pane_run_reads_label_timeout_and_body() {
    let a: Vec<String> = ["-l", "logs", "--timeout", "5", "tail", "-f", "x"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (label, cmd, timeout) = parse_pane_run(&a).unwrap();
    assert_eq!(label.as_deref(), Some("logs"));
    assert_eq!(cmd, "tail -f x"); // a `-f` in the body is not a flag
    assert_eq!(timeout, 5);

    let b: Vec<String> = vec!["uptime".to_string()];
    let (label2, cmd2, t2) = parse_pane_run(&b).unwrap();
    assert!(label2.is_none());
    assert_eq!(cmd2, "uptime");
    assert_eq!(t2, runcmd::DEFAULT_RUN_TIMEOUT_SECS);

    assert!(parse_pane_run(&Vec::new()).is_err());
    assert!(parse_pane_run(&["a\nb".to_string()]).is_err());
    let bad: Vec<String> = ["--timeout", "0", "x"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(parse_pane_run(&bad).is_err());
}

#[test]
fn viewport_command_embeds_socket_when_present() {
    assert_eq!(
        viewport_command(Some("/tmp/a.sock"), "web"),
        "SSH_AUTH_SOCK=/tmp/a.sock helm shell open web"
    );
    // Empty / absent socket → no SSH_AUTH_SOCK prefix.
    assert_eq!(viewport_command(Some(""), "web"), "helm shell open web");
    assert_eq!(viewport_command(None, "web"), "helm shell open web");
    // A target with a label is quoted intact.
    assert_eq!(
        viewport_command(None, "web:diag"),
        "helm shell open web:diag"
    );
}

#[test]
fn render_pane_list_keeps_only_tagged_panes() {
    let raw = "%0\t\t\t\n%1\thelm\t\t\n%2\t\tweb\t\n%3\thelm-logs\t\t\n%4\t\t\tbuild\n";
    let rows = render_pane_list(raw);
    assert_eq!(
        rows,
        vec![
            "helm\tdrivable\t%1".to_string(),
            "web\tviewport\t%2".to_string(),
            "helm-logs\tdrivable\t%3".to_string(),
            "build\tbackground\t%4".to_string(),
        ]
    );
    assert!(render_pane_list("").is_empty());
}

#[test]
fn window_has_helm_pane_detects_any_tagged_pane() {
    // Orphaned window: a "helm"-titled pane with EMPTY label and viewport
    // (the live-caught bug) counts as NO helm pane — markers must drop.
    assert!(!window_has_helm_pane("\t\n\t\n"));
    assert!(!window_has_helm_pane(""));
    // A drivable pane keeps the markers justified.
    assert!(window_has_helm_pane("\t\nhelm\t\n"));
    // A viewport alone also justifies them.
    assert!(window_has_helm_pane("\tweb\n"));
    // A background pane alone also justifies them (keeps @helm_here alive
    // while a pi-bg job runs, even with no drivable/viewport pane).
    assert!(window_has_helm_pane("\t\tbuild\n"));
    // Whitespace-only fields don't count.
    assert!(!window_has_helm_pane("   \t  \t  \n"));
}

#[test]
fn label_tag_maps_like_the_skill() {
    assert_eq!(label_tag(None).unwrap(), "helm");
    assert_eq!(label_tag(Some("")).unwrap(), "helm");
    assert_eq!(label_tag(Some("logs")).unwrap(), "helm-logs");
    assert!(label_tag(Some("a:b")).is_err());
}

#[test]
fn tag_filter_builds_the_tmux_predicate() {
    assert_eq!(
        tag_filter("@helm_label", "helm"),
        "#{==:#{@helm_label},helm}"
    );
    assert_eq!(
        tag_filter("@helm_viewport", "web"),
        "#{==:#{@helm_viewport},web}"
    );
}

#[test]
fn parse_opts_reads_flags_and_positionals() {
    let a: Vec<String> = ["-l", "logs", "--size", "30", "--below", "web"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let o = parse_opts(&a).unwrap();
    assert_eq!(o.label.as_deref(), Some("logs"));
    assert_eq!(o.size, Some(30));
    assert!(o.below);
    assert_eq!(o.positional, vec!["web".to_string()]);
}

#[test]
fn parse_opts_read_flags() {
    let a: Vec<String> = ["-n", "50", "--raw"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let o = parse_opts(&a).unwrap();
    assert_eq!(o.lines, Some(50));
    assert!(o.raw);
}

#[test]
fn parse_opts_rejects_missing_value() {
    let a: Vec<String> = vec!["-l".to_string()];
    assert!(parse_opts(&a).is_err());
}

#[test]
fn parse_opts_rejects_bad_numbers() {
    let bad_size: Vec<String> = ["--size", "x"].iter().map(|s| s.to_string()).collect();
    assert!(parse_opts(&bad_size).is_err());
    let bad_n: Vec<String> = ["-n", "nope"].iter().map(|s| s.to_string()).collect();
    assert!(parse_opts(&bad_n).is_err());
    // `-n 0` would become `-S -0` (the whole visible pane), not "0 lines";
    // reject it like a non-numeric value so the message holds true.
    let zero_n: Vec<String> = ["-n", "0"].iter().map(|s| s.to_string()).collect();
    assert!(parse_opts(&zero_n).is_err());
    let missing: Vec<String> = vec!["--size".to_string()];
    assert!(parse_opts(&missing).is_err());
}

#[test]
fn split_leading_label_consumes_only_a_leading_flag() {
    let a: Vec<String> = ["-l", "logs", "tail", "-f", "x"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (label, rest) = split_leading_label(&a).unwrap();
    assert_eq!(label.as_deref(), Some("logs"));
    // A `-f` in the body is preserved verbatim, not parsed as a flag.
    assert_eq!(
        rest,
        &["tail".to_string(), "-f".to_string(), "x".to_string()]
    );
}

#[test]
fn split_leading_label_absent_keeps_all_args() {
    let a: Vec<String> = ["echo", "-n", "hi"].iter().map(|s| s.to_string()).collect();
    let (label, rest) = split_leading_label(&a).unwrap();
    assert!(label.is_none());
    assert_eq!(rest.len(), 3);
}

#[test]
fn cmd_wait_rejects_bad_args_before_touching_tmux() {
    // Both fail in the pure parse, before any pane resolution.
    let a: Vec<String> = ["--timeout"].iter().map(|s| s.to_string()).collect();
    assert!(cmd_wait(&a).is_err());
    let a: Vec<String> = ["bogus"].iter().map(|s| s.to_string()).collect();
    assert!(cmd_wait(&a).is_err());
    let a: Vec<String> = ["--timeout", "0"].iter().map(|s| s.to_string()).collect();
    assert!(cmd_wait(&a).is_err());
}
