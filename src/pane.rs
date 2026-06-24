//! `helm pane` — drive panes inside helm's *own* tmux window (the window
//! helm is running in, found via `$TMUX_PANE`), rather than a tmux session on
//! a host. Two kinds of pane live here:
//!
//! - a **drivable** pane (tagged `@helm_label`): a local shell helm types into
//!   — the in-window successor to a "here" shell.
//! - a **viewport** pane (tagged `@helm_viewport`): a read-only client attached
//!   to a remote helm session so the operator watches live; helm never types
//!   into it.
//!
//! Everything here is raw `tmux` against the operator's own server — no ssh.
//! The pane markers, the window flag `@helm_here`, and the `pane-border-format`
//! string are kept byte-for-byte identical to what the helm-shell skill has
//! always documented, because the operator's `~/.tmux.conf` (a
//! `window-layout-changed` cleanup hook, a status-bar `@helm_here` fragment) is
//! a contract keyed on those exact names. `close` only kills the pane and lets
//! that hook drop the markers — which also covers panes closed by hand — with a
//! belt-and-suspenders teardown for configs that lack the hook.

use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use crate::config::Config;
use crate::tmux::{self, strip_trailing_blank};

/// The window border format. IDENTICAL to the helm-shell skill's — the
/// operator's tmux config renders `@helm_label`/`@helm_viewport` with it, so
/// it must not drift.
const BORDER_FORMAT: &str = "#{?#{@helm_label}, #[fg=cyan]⚓ #{@helm_label}#[default] ,#{?#{@helm_viewport}, #[fg=yellow]👁 #{@helm_viewport}#[default] , #{pane_index}: #{pane_title} }}";

pub(crate) fn run_cli(args: &[String]) -> ExitCode {
    let Some(sub) = args.first() else {
        print_help();
        return ExitCode::from(2);
    };
    if matches!(sub.as_str(), "help" | "--help" | "-h") {
        print_help();
        return ExitCode::SUCCESS;
    }
    // Match the shell CLI: load config once so tmux calls carry the configured
    // global flags (e.g. `-u`), addressing the operator's own server.
    let flags = Config::load_silent()
        .map(|c| c.tmux_flags())
        .unwrap_or_else(|_| Config::default().tmux_flags());
    tmux::set_flags(flags);

    let rest = &args[1..];
    let result = match sub.as_str() {
        "open" => cmd_open(rest),
        "view" => cmd_view(rest),
        "send" => cmd_send(rest),
        "key" => cmd_key(rest),
        "read" => cmd_read(rest),
        "close" => cmd_close(rest),
        "list" => cmd_list(rest),
        other => {
            eprintln!("helm pane: unknown subcommand `{other}`");
            print_help();
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("helm: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    eprintln!(
        "helm pane — drive panes in helm's own tmux window (requires $TMUX_PANE)

Panes live in the window helm is running in, on the operator's own tmux
server. A drivable pane is a local shell helm types into; a viewport is a
read-only view of a remote helm session. `-l LABEL` names a second pane
(default label is the bare `helm` pane); it maps to tmux option @helm_label
= helm[-LABEL], the same tag the skill documents.

usage:
  helm pane open [-l LABEL] [--below] [--size N]   resolve-or-create a
                                                   drivable pane
  helm pane view <target> [--below] [--size N]     resolve-or-create a
                                                   read-only viewport for a
                                                   remote helm session
  helm pane send [-l LABEL] <text...>              type a line (auto-Enter)
  helm pane key  [-l LABEL] <key...>               send raw key specs (Up,
                                                   C-c, Escape; no Enter)
  helm pane read [-l LABEL] [-n N] [--raw]         capture the pane (default
                                                   200 lines, trailing blanks
                                                   stripped unless --raw)
  helm pane close [-l LABEL]                        kill the drivable pane
  helm pane list                                   list helm panes here

Close a viewport by killing its remote session with `helm shell close <target>`."
    );
}

// ---- tmux helpers (local server) -------------------------------------------

/// Run a local `tmux` command and return its stdout.
fn tmux_capture(args: &[&str]) -> Result<String> {
    let out = Command::new("tmux")
        .args(tmux::flags())
        .args(args)
        .stderr(Stdio::piped())
        .output()
        .context("spawn tmux")?;
    if !out.status.success() {
        return Err(anyhow!(
            "tmux {} failed (exit {}): {}",
            args.join(" "),
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run a local `tmux` command for its side effect.
fn tmux_act(args: &[&str]) -> Result<()> {
    let status = Command::new("tmux")
        .args(tmux::flags())
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("spawn tmux")?;
    if !status.success() {
        return Err(anyhow!(
            "tmux {} failed (exit {})",
            args.join(" "),
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// The pane helm is running in. Errors (rather than guessing a window) when
/// `$TMUX_PANE` is unset — helm isn't inside the operator's tmux then.
fn current_pane() -> Result<String> {
    std::env::var("TMUX_PANE")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "not inside tmux ($TMUX_PANE unset). `helm pane` drives a pane in helm's own \
                 tmux window; from outside tmux, use `helm shell open <target>` instead."
            )
        })
}

fn window_of(pane: &str) -> Result<String> {
    Ok(
        tmux_capture(&["display-message", "-p", "-t", pane, "#{window_id}"])?
            .trim()
            .to_string(),
    )
}

/// Map an optional `-l LABEL` to the tmux tag value: bare → `helm`, `logs` →
/// `helm-logs`. Mirrors the skill's `here`/`here:logs` → `@helm_label`.
fn label_tag(label: Option<&str>) -> String {
    match label {
        Some(l) if !l.is_empty() => format!("helm-{l}"),
        _ => "helm".to_string(),
    }
}

/// Build the tmux `-f` filter that matches a pane carrying `opt == val`, e.g.
/// `#{==:#{@helm_label},helm}`. Split out so the brace-escaping is testable.
fn tag_filter(opt: &str, val: &str) -> String {
    format!("#{{==:#{{{opt}}},{val}}}")
}

/// First pane in `win` whose user option `opt` equals `val`, if any.
fn find_tagged(win: &str, opt: &str, val: &str) -> Result<Option<String>> {
    let filter = tag_filter(opt, val);
    let out = tmux_capture(&["list-panes", "-t", win, "-f", &filter, "-F", "#{pane_id}"])?;
    Ok(out
        .lines()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(String::from))
}

/// Install the window-level markers the operator's tmux config renders.
fn set_window_markers(win: &str) -> Result<()> {
    tmux_act(&["set-option", "-w", "-t", win, "@helm_here", "1"])?;
    tmux_act(&["set-option", "-w", "-t", win, "pane-border-status", "top"])?;
    tmux_act(&[
        "set-option",
        "-w",
        "-t",
        win,
        "pane-border-format",
        BORDER_FORMAT,
    ])?;
    Ok(())
}

/// Split a new pane off `anchor`, returning its pane id. `pane_cmd` is the
/// trailing command tmux runs in the pane (a viewport's attach); `None` leaves
/// a default shell.
fn split(anchor: &str, below: bool, size: Option<u32>, pane_cmd: Option<&str>) -> Result<String> {
    let mut v: Vec<String> = vec![
        "split-window".into(),
        "-d".into(),
        if below { "-v".into() } else { "-h".into() },
        "-t".into(),
        anchor.into(),
    ];
    if let Some(s) = size {
        v.push("-l".into());
        v.push(s.to_string());
    }
    v.push("-P".into());
    v.push("-F".into());
    v.push("#{pane_id}".into());
    if let Some(cmd) = pane_cmd {
        v.push(cmd.into());
    }
    let refs: Vec<&str> = v.iter().map(String::as_str).collect();
    Ok(tmux_capture(&refs)?.trim().to_string())
}

/// Resolve the drivable pane for `tag`, creating it (and the window markers) if
/// absent — the in-window analogue of `helm shell`'s auto-create on use.
fn ensure_drivable(
    win: &str,
    anchor: &str,
    tag: &str,
    below: bool,
    size: Option<u32>,
) -> Result<String> {
    if let Some(p) = find_tagged(win, "@helm_label", tag)? {
        return Ok(p);
    }
    let pane = split(anchor, below, size, None)?;
    tmux_act(&["set-option", "-p", "-t", &pane, "@helm_label", tag])?;
    tmux_act(&["select-pane", "-t", &pane, "-T", tag])?;
    set_window_markers(win)?;
    Ok(pane)
}

/// Resolve the viewport pane for `target`, creating it if absent. The pane's
/// command attaches the remote session; helm's current `SSH_AUTH_SOCK` is
/// embedded so the child ssh can authenticate (the spawned pane does not
/// inherit helm's env).
fn ensure_viewport(
    win: &str,
    anchor: &str,
    target: &str,
    below: bool,
    size: Option<u32>,
) -> Result<String> {
    if let Some(p) = find_tagged(win, "@helm_viewport", target)? {
        return Ok(p);
    }
    let cmd = match std::env::var("SSH_AUTH_SOCK")
        .ok()
        .filter(|s| !s.is_empty())
    {
        Some(sock) => format!(
            "SSH_AUTH_SOCK={} helm shell open {}",
            tmux::shell_quote(&sock),
            tmux::shell_quote(target)
        ),
        None => format!("helm shell open {}", tmux::shell_quote(target)),
    };
    let pane = split(anchor, below, size, Some(&cmd))?;
    tmux_act(&["set-option", "-p", "-t", &pane, "@helm_viewport", target])?;
    tmux_act(&["select-pane", "-t", &pane, "-T", target])?;
    set_window_markers(win)?;
    Ok(pane)
}

// ---- argument parsing ------------------------------------------------------

#[derive(Default, Debug, PartialEq, Eq)]
struct Opts {
    label: Option<String>,
    below: bool,
    size: Option<u32>,
    lines: Option<u32>,
    raw: bool,
    positional: Vec<String>,
}

fn take_val(args: &[String], i: &mut usize, flag: &str) -> Result<String> {
    let v = args
        .get(*i + 1)
        .cloned()
        .ok_or_else(|| anyhow!("{flag} requires a value"))?;
    *i += 2;
    Ok(v)
}

/// Full flag scan — for subcommands whose positionals can't begin with `-`
/// (open/view/read/close). `send`/`key` use `split_leading_label` instead so a
/// `-`-leading word in the body isn't eaten as a flag.
fn parse_opts(args: &[String]) -> Result<Opts> {
    let mut o = Opts::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-l" | "--label" => o.label = Some(take_val(args, &mut i, "-l")?),
            "--below" => {
                o.below = true;
                i += 1;
            }
            "--size" => {
                let v = take_val(args, &mut i, "--size")?;
                o.size = Some(
                    v.parse()
                        .map_err(|_| anyhow!("--size requires a positive integer"))?,
                );
            }
            "-n" => {
                let v = take_val(args, &mut i, "-n")?;
                o.lines = Some(
                    v.parse()
                        .map_err(|_| anyhow!("-n requires a positive integer"))?,
                );
            }
            "--raw" => {
                o.raw = true;
                i += 1;
            }
            other => {
                o.positional.push(other.to_string());
                i += 1;
            }
        }
    }
    Ok(o)
}

/// Consume a leading `-l LABEL` (only), returning the label and the verbatim
/// rest — the body for `send`/`key`, which may itself contain `-` words.
fn split_leading_label(args: &[String]) -> Result<(Option<String>, &[String])> {
    if let Some(first) = args.first()
        && (first == "-l" || first == "--label")
    {
        let label = args
            .get(1)
            .ok_or_else(|| anyhow!("-l requires a label"))?
            .clone();
        return Ok((Some(label), &args[2..]));
    }
    Ok((None, args))
}

// ---- subcommands -----------------------------------------------------------

fn cmd_open(args: &[String]) -> Result<ExitCode> {
    let o = parse_opts(args)?;
    if !o.positional.is_empty() {
        bail!("usage: helm pane open [-l LABEL] [--below] [--size N]");
    }
    let anchor = current_pane()?;
    let win = window_of(&anchor)?;
    let tag = label_tag(o.label.as_deref());
    let pane = ensure_drivable(&win, &anchor, &tag, o.below, o.size)?;
    eprintln!("helm: pane {tag} ready ({pane}) in this window");
    Ok(ExitCode::SUCCESS)
}

fn cmd_view(args: &[String]) -> Result<ExitCode> {
    let o = parse_opts(args)?;
    let Some(target) = o.positional.first() else {
        bail!("usage: helm pane view <target> [--below] [--size N]");
    };
    let anchor = current_pane()?;
    let win = window_of(&anchor)?;
    let pane = ensure_viewport(&win, &anchor, target, o.below, o.size)?;
    eprintln!("helm: viewport for {target} ({pane}) in this window");
    Ok(ExitCode::SUCCESS)
}

fn cmd_send(args: &[String]) -> Result<ExitCode> {
    let (label, rest) = split_leading_label(args)?;
    if rest.is_empty() {
        bail!("usage: helm pane send [-l LABEL] <text...>");
    }
    let text = rest.join(" ");
    let anchor = current_pane()?;
    let win = window_of(&anchor)?;
    let tag = label_tag(label.as_deref());
    let pane = ensure_drivable(&win, &anchor, &tag, false, None)?;
    // `--` so a body starting with `-` is literal keys, not a send-keys flag.
    tmux_act(&["send-keys", "-t", &pane, "-l", "--", &text])?;
    tmux_act(&["send-keys", "-t", &pane, "Enter"])?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_key(args: &[String]) -> Result<ExitCode> {
    let (label, rest) = split_leading_label(args)?;
    if rest.is_empty() {
        bail!("usage: helm pane key [-l LABEL] <key...>  (e.g. Up Up Enter, C-c)");
    }
    let anchor = current_pane()?;
    let win = window_of(&anchor)?;
    let tag = label_tag(label.as_deref());
    let pane = ensure_drivable(&win, &anchor, &tag, false, None)?;
    let mut v: Vec<String> = vec!["send-keys".into(), "-t".into(), pane.clone(), "--".into()];
    v.extend(rest.iter().cloned());
    let refs: Vec<&str> = v.iter().map(String::as_str).collect();
    tmux_act(&refs)?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_read(args: &[String]) -> Result<ExitCode> {
    let o = parse_opts(args)?;
    let anchor = current_pane()?;
    let win = window_of(&anchor)?;
    let tag = label_tag(o.label.as_deref());
    let pane = ensure_drivable(&win, &anchor, &tag, false, None)?;
    let n = o.lines.unwrap_or(tmux::DEFAULT_CAPTURE_LINES);
    let neg = format!("-{n}");
    let raw_out = tmux_capture(&["capture-pane", "-t", &pane, "-p", "-S", &neg])?;
    if o.raw {
        print!("{raw_out}");
    } else {
        println!("{}", strip_trailing_blank(&raw_out));
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_close(args: &[String]) -> Result<ExitCode> {
    let o = parse_opts(args)?;
    let anchor = current_pane()?;
    let win = window_of(&anchor)?;
    let tag = label_tag(o.label.as_deref());
    let Some(pane) = find_tagged(&win, "@helm_label", &tag)? else {
        bail!("no helm pane labelled {tag} in this window");
    };
    tmux_act(&["kill-pane", "-t", &pane])?;
    // The operator's `window-layout-changed` hook owns marker teardown (it
    // fires for hand-closed panes too). Mirror it here for configs without the
    // hook: if no tagged pane (drivable or viewport) remains, drop the markers.
    let remaining = tmux_capture(&[
        "list-panes",
        "-t",
        &win,
        "-f",
        "#{||:#{!=:#{@helm_label},},#{!=:#{@helm_viewport},}}",
        "-F",
        "x",
    ])?;
    if remaining.trim().is_empty() {
        for opt in ["@helm_here", "pane-border-status", "pane-border-format"] {
            let _ = tmux_act(&["set-option", "-w", "-t", &win, "-u", opt]);
        }
    }
    eprintln!("helm: closed pane {tag}");
    Ok(ExitCode::SUCCESS)
}

fn cmd_list(_args: &[String]) -> Result<ExitCode> {
    let anchor = current_pane()?;
    let win = window_of(&anchor)?;
    let out = tmux_capture(&[
        "list-panes",
        "-t",
        &win,
        "-F",
        "#{pane_id}\t#{@helm_label}\t#{@helm_viewport}",
    ])?;
    let mut any = false;
    for line in out.lines() {
        let mut it = line.splitn(3, '\t');
        let id = it.next().unwrap_or("");
        let label = it.next().unwrap_or("");
        let view = it.next().unwrap_or("");
        if !label.is_empty() {
            println!("{label}\tdrivable\t{id}");
            any = true;
        } else if !view.is_empty() {
            println!("{view}\tviewport\t{id}");
            any = true;
        }
    }
    if !any {
        eprintln!("(no helm panes in this window)");
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_tag_maps_like_the_skill() {
        assert_eq!(label_tag(None), "helm");
        assert_eq!(label_tag(Some("")), "helm");
        assert_eq!(label_tag(Some("logs")), "helm-logs");
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
}
