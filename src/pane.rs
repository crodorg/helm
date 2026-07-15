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
//! string are kept byte-for-byte identical to what the helm skill has
//! always documented, because the operator's `~/.tmux.conf` (a
//! `window-layout-changed` cleanup hook, a status-bar `@helm_here` fragment) is
//! a contract keyed on those exact names. `close` kills the pane and then
//! reconciles the markers UNCONDITIONALLY (see `sweep_markers`): whether or not
//! a labelled pane was found, if no tagged pane remains it drops `@helm_here`
//! and the border options. That belt-and-suspenders teardown covers configs
//! lacking the hook and, crucially, the case the hook can't catch — a pane
//! closed/untagged by hand, where no layout change fires. `reconcile` exposes
//! the same sweep as a standalone verb to self-heal an orphaned ⚓ anchor.

use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use crate::activity::ActivityKind;
use crate::config::Config;
use crate::opts;
use crate::readcursor;
use crate::runcmd::{self, strip_trailing_blank};
use crate::tmux;
use crate::watch;

/// Append one audit record for a pane action, mirroring `helm shell`. Pane work
/// is local (no ssh), so the alias slot is the literal `pane` and the session
/// slot is the pane tag; `cmd` carries the command so the privilege-escalation
/// flag is computed for `run`/`send` just as it is for a remote shell.
fn log_pane(kind: ActivityKind, tag: &str, cmd: &str, exit: Option<i32>) {
    crate::log_action(kind, "pane", tag, cmd, "", exit);
}

/// The window border format. IDENTICAL to the helm skill's AND the
/// `pi-bg` extension's copy (which sets it so background panes render ⚙ even
/// when no helm-managed pane exists) — the operator's tmux config renders
/// `@helm_label`/`@helm_viewport`/`@helm_bg` with it, so it must not drift.
const BORDER_FORMAT: &str = "#{?#{@helm_label}, #[fg=cyan]⚓ #{@helm_label}#[default] ,#{?#{@helm_viewport}, #[fg=yellow]👁 #{@helm_viewport}#[default] ,#{?#{@helm_bg}, #[fg=magenta]⚙ #{@helm_bg}#[default] , #{pane_index}: #{pane_title} }}}";

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
        "run" => cmd_run(rest),
        "wait" => cmd_wait(rest),
        "watch" => cmd_watch(rest),
        "key" => cmd_key(rest),
        "read" => cmd_read(rest),
        "close" => cmd_close(rest),
        "reconcile" => cmd_reconcile(rest),
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
  helm pane run  [-l LABEL] <cmd...>               run one command; print its
   [--timeout SECS]                                output + `exit: N`
  helm pane wait [-l LABEL] [--timeout SECS]       block until the pane is
                                                   back at a shell prompt
                                                   (default 60s; no exit code
                                                   — pair with send + read
                                                   --delta)
  helm pane watch [-l LABEL]                       block until a predicate
   [--idle | --match REGEX] [--timeout SECS]       holds, then park. --idle =
                                                   back at a prompt (same as
                                                   wait); --match = a line
                                                   matching REGEX (grep -E)
                                                   appears in NEW output.
                                                   Exit 0/124/1
  helm pane key  [-l LABEL] <key...>               send raw key specs (Up,
                                                   C-c, Escape; no Enter)
  helm pane read [-l LABEL] [-n N] [--raw|--delta] capture the pane (default
                                                   200 lines, trailing blanks
                                                   stripped unless --raw).
                                                   --delta prints only lines
                                                   NEW since the previous
                                                   --delta read (-n caps them)
  helm pane close [-l LABEL]                        kill the drivable pane
  helm pane reconcile                              clear an orphaned ⚓ anchor
                                                   (no pane left to justify it)
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
fn label_tag(label: Option<&str>) -> Result<String> {
    match label {
        Some(l) if !l.is_empty() => {
            if !crate::tmux::valid_label(l) {
                bail!("label must be [A-Za-z0-9._-]: `{l}`");
            }
            Ok(format!("helm-{l}"))
        }
        _ => Ok("helm".to_string()),
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

/// True when `list-panes` output (one `@helm_label<TAB>@helm_viewport<TAB>@helm_bg`
/// row per pane) still contains at least one tagged pane — i.e. the window
/// markers are still justified. Includes background (`@helm_bg`) panes so a
/// running pi-bg job keeps `@helm_here` (and thus the border) alive even when
/// no drivable/viewport pane remains. Pure so the teardown decision is testable.
fn window_has_helm_pane(raw: &str) -> bool {
    raw.lines().any(|line| {
        let mut it = line.splitn(3, '\t');
        let label = it.next().unwrap_or("").trim();
        let view = it.next().unwrap_or("").trim();
        let bg = it.next().unwrap_or("").trim();
        !label.is_empty() || !view.is_empty() || !bg.is_empty()
    })
}

/// Reconcile the window markers with reality: if no tagged pane (drivable,
/// viewport, or background) remains in `win`, drop `@helm_here` and the border options so the
/// operator's status bar stops drawing an orphaned ⚓ anchor. Returns whether
/// the markers were cleared. Idempotent — safe to call when nothing is stale.
///
/// This mirrors the operator's `window-layout-changed` hook for configs that
/// lack it, and — crucially — runs on `close` even when the labelled pane is
/// already gone or untagged, which is exactly the case the hook can't catch
/// (losing a pane option isn't a layout change, so the hook never fires).
fn sweep_markers(win: &str) -> Result<bool> {
    let raw = tmux_capture(&[
        "list-panes",
        "-t",
        win,
        "-F",
        "#{@helm_label}\t#{@helm_viewport}\t#{@helm_bg}",
    ])?;
    if window_has_helm_pane(&raw) {
        return Ok(false);
    }
    for opt in ["@helm_here", "pane-border-status", "pane-border-format"] {
        let _ = tmux_act(&["set-option", "-w", "-t", win, "-u", opt]);
    }
    Ok(true)
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
    let sock = std::env::var("SSH_AUTH_SOCK").ok();
    let cmd = viewport_command(sock.as_deref(), target);
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
    delta: bool,
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
                o.lines = Some(opts::parse_lines(&v).map_err(|e| anyhow!(e))?);
            }
            "--raw" => {
                o.raw = true;
                i += 1;
            }
            "--delta" => {
                o.delta = true;
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
    let tag = label_tag(o.label.as_deref())?;
    let pane = ensure_drivable(&win, &anchor, &tag, o.below, o.size)?;
    log_pane(ActivityKind::ShellOpen, &tag, "open", Some(0));
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
    log_pane(
        ActivityKind::ShellOpen,
        target,
        &format!("view {target}"),
        Some(0),
    );
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
    let tag = label_tag(label.as_deref())?;
    let pane = ensure_drivable(&win, &anchor, &tag, false, None)?;
    // `--` so a body starting with `-` is literal keys, not a send-keys flag.
    tmux_act(&["send-keys", "-t", &pane, "-l", "--", &text])?;
    tmux_act(&["send-keys", "-t", &pane, "Enter"])?;
    log_pane(ActivityKind::ShellSend, &tag, &text, Some(0));
    Ok(ExitCode::SUCCESS)
}

/// Parse `helm pane run [-l LABEL] [--timeout SECS] <cmd...>`. Pure for tests.
/// The label is a leading flag only (so a `-` in the body is literal); the
/// command is single-line and non-interactive, like `helm shell run`.
fn parse_pane_run(args: &[String]) -> Result<(Option<String>, String, u32)> {
    let (label, rest) = split_leading_label(args)?;
    let mut timeout = runcmd::DEFAULT_RUN_TIMEOUT_SECS;
    let mut cmd_parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == "--timeout" {
            let val = rest
                .get(i + 1)
                .ok_or_else(|| anyhow!("--timeout requires a value (seconds)"))?;
            timeout = opts::parse_timeout(val).map_err(|e| anyhow!(e))?;
            i += 2;
        } else {
            cmd_parts.push(rest[i].clone());
            i += 1;
        }
    }
    let cmd = opts::single_line_command(&cmd_parts).map_err(|e| match e {
        opts::CommandError::Empty => {
            anyhow!("usage: helm pane run [-l LABEL] [--timeout SECS] <cmd...>")
        }
        opts::CommandError::MultiLine => {
            anyhow!("helm pane run: command must be a single line (no newlines)")
        }
    })?;
    Ok((label, cmd, timeout))
}

fn cmd_run(args: &[String]) -> Result<ExitCode> {
    let (label, cmd, timeout) = parse_pane_run(args)?;
    let anchor = current_pane()?;
    let win = window_of(&anchor)?;
    let tag = label_tag(label.as_deref())?;
    let pane = ensure_drivable(&win, &anchor, &tag, false, None)?;
    let outcome = runcmd::run_in_pane(&pane, &cmd, timeout)?;
    if outcome.busy {
        log_pane(ActivityKind::ShellRun, &tag, &cmd, Some(1));
        eprintln!(
            "helm: pane busy (a command/editor/REPL is running, not a shell prompt). \
             Use `send`/`read`, or `key`."
        );
        return Ok(ExitCode::FAILURE);
    }
    if outcome.gone {
        log_pane(ActivityKind::ShellRun, &tag, &cmd, Some(1));
        eprintln!(
            "helm: the command terminated the pane's shell ({tag} is gone). \
             Reopen with `helm pane open`."
        );
        return Ok(ExitCode::FAILURE);
    }
    log_pane(ActivityKind::ShellRun, &tag, &cmd, outcome.exit);
    if !outcome.output.is_empty() {
        println!("{}", outcome.output);
    }
    match outcome.exit {
        Some(code) => eprintln!("exit: {code}"),
        None => eprintln!(
            "helm: command still running after {timeout}s (timeout). \
             Poll with `helm pane read`."
        ),
    }
    Ok(ExitCode::from(opts::run_exit_byte(
        false,
        false,
        outcome.exit,
    )))
}

fn cmd_wait(args: &[String]) -> Result<ExitCode> {
    let (label, rest) = split_leading_label(args)?;
    let timeout =
        opts::parse_wait_timeout(rest, watch::DEFAULT_WATCH_TIMEOUT_SECS).map_err(|e| {
            anyhow!("helm pane wait: {e}\nusage: helm pane wait [-l LABEL] [--timeout SECS]")
        })?;
    // `wait` is `watch --idle` under a stable name.
    run_pane_watch(label, &watch::Predicate::Idle, timeout)
}

fn cmd_watch(args: &[String]) -> Result<ExitCode> {
    let (label, rest) = split_leading_label(args)?;
    let (pred, timeout) =
        watch::parse_args(rest, watch::DEFAULT_WATCH_TIMEOUT_SECS).map_err(|e| {
            anyhow!(
                "helm pane watch: {e}\nusage: helm pane watch [-l LABEL] [--idle | --match REGEX] [--timeout SECS]"
            )
        })?;
    run_pane_watch(label, &pred, timeout)
}

/// Shared engine for `pane wait` (idle) and `pane watch` (any predicate):
/// resolve the pane (never create), block host-side, log, and report.
fn run_pane_watch(
    label: Option<String>,
    pred: &watch::Predicate,
    timeout: u32,
) -> Result<ExitCode> {
    let anchor = current_pane()?;
    let win = window_of(&anchor)?;
    let tag = label_tag(label.as_deref())?;
    let kind = match pred {
        watch::Predicate::Idle => ActivityKind::ShellWait,
        watch::Predicate::Match(_) => ActivityKind::ShellWatch,
    };
    // Resolve only — never create: watching a pane that doesn't exist would
    // conjure a fresh idle shell and report a meaningless "done".
    let Some(pane) = find_tagged(&win, "@helm_label", &tag)? else {
        log_pane(kind, &tag, "", Some(1));
        eprintln!("helm: no drivable pane `{tag}` in this window — open one with `helm pane open`");
        return Ok(ExitCode::FAILURE);
    };
    let outcome = watch::watch_pane(&pane, pred, timeout)?;
    let (logged, exit) = outcome.logged();
    log_pane(kind, &tag, logged, exit);
    eprintln!(
        "helm: {}",
        outcome.report(
            pred,
            &format!("pane {tag}"),
            timeout,
            "helm pane read --delta",
            "helm pane open",
        )
    );
    Ok(ExitCode::from(outcome.exit_byte()))
}

fn cmd_key(args: &[String]) -> Result<ExitCode> {
    let (label, rest) = split_leading_label(args)?;
    if rest.is_empty() {
        bail!("usage: helm pane key [-l LABEL] <key...>  (e.g. Up Up Enter, C-c)");
    }
    let anchor = current_pane()?;
    let win = window_of(&anchor)?;
    let tag = label_tag(label.as_deref())?;
    let pane = ensure_drivable(&win, &anchor, &tag, false, None)?;
    let mut v: Vec<String> = vec!["send-keys".into(), "-t".into(), pane.clone(), "--".into()];
    v.extend(rest.iter().cloned());
    let refs: Vec<&str> = v.iter().map(String::as_str).collect();
    tmux_act(&refs)?;
    log_pane(ActivityKind::ShellKey, &tag, &rest.join(" "), Some(0));
    Ok(ExitCode::SUCCESS)
}

fn cmd_read(args: &[String]) -> Result<ExitCode> {
    let o = parse_opts(args)?;
    let anchor = current_pane()?;
    let win = window_of(&anchor)?;
    let tag = label_tag(o.label.as_deref())?;
    let pane = ensure_drivable(&win, &anchor, &tag, false, None)?;
    let n = o.lines.unwrap_or(tmux::DEFAULT_CAPTURE_LINES);
    if o.delta {
        if o.raw {
            bail!("--delta and --raw are mutually exclusive");
        }
        let key = readcursor::key_pane(&pane);
        let res = readcursor::delta_read(tmux::LOCAL_ALIAS, &pane, &key, n)?;
        if let Some(note) = res.note(n) {
            eprintln!("helm: {note}");
        }
        if !res.stdout().is_empty() {
            println!("{}", res.stdout());
        }
        log_pane(ActivityKind::ShellRead, &tag, "--delta", Some(0));
        return Ok(ExitCode::SUCCESS);
    }
    let neg = format!("-{n}");
    let raw_out = tmux_capture(&["capture-pane", "-t", &pane, "-p", "-S", &neg])?;
    if o.raw {
        print!("{raw_out}");
    } else {
        println!("{}", strip_trailing_blank(&raw_out));
    }
    log_pane(ActivityKind::ShellRead, &tag, "", Some(0));
    Ok(ExitCode::SUCCESS)
}

fn cmd_close(args: &[String]) -> Result<ExitCode> {
    let o = parse_opts(args)?;
    let anchor = current_pane()?;
    let win = window_of(&anchor)?;
    let tag = label_tag(o.label.as_deref())?;
    // Kill the labelled pane if it's still there, but reconcile the window
    // markers UNCONDITIONALLY. If the pane is already gone or untagged, we must
    // not bail before the sweep — that's the orphaned-⚓ bug: `@helm_here` stays
    // set with no pane to justify it, and the layout-changed hook can't clear
    // it (a lost pane option isn't a layout change).
    match find_tagged(&win, "@helm_label", &tag)? {
        Some(pane) => {
            tmux_act(&["kill-pane", "-t", &pane])?;
            sweep_markers(&win)?;
            log_pane(ActivityKind::ShellClose, &tag, "close", Some(0));
            eprintln!("helm: closed pane {tag}");
        }
        None => {
            let cleared = sweep_markers(&win)?;
            log_pane(ActivityKind::ShellClose, &tag, "close", Some(0));
            if cleared {
                eprintln!("helm: no pane {tag}; cleared orphaned window markers");
            } else {
                eprintln!("helm: no pane labelled {tag} in this window");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Reconcile the current window's markers with reality — self-heal a window
/// whose ⚓ anchor was orphaned (e.g. a helm pane closed by hand on a config
/// without the `window-layout-changed` hook). No panes are touched.
fn cmd_reconcile(_args: &[String]) -> Result<ExitCode> {
    let anchor = current_pane()?;
    let win = window_of(&anchor)?;
    let cleared = sweep_markers(&win)?;
    log_pane(ActivityKind::ShellClose, "", "reconcile", Some(0));
    if cleared {
        eprintln!("helm: cleared orphaned window markers");
    } else {
        eprintln!("helm: markers already consistent");
    }
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
        "#{pane_id}\t#{@helm_label}\t#{@helm_viewport}\t#{@helm_bg}",
    ])?;
    let rows = render_pane_list(&out);
    if rows.is_empty() {
        eprintln!("(no helm panes in this window)");
    } else {
        for r in &rows {
            println!("{r}");
        }
    }
    log_pane(ActivityKind::ShellList, "", "", Some(0));
    Ok(ExitCode::SUCCESS)
}

/// Build the viewport pane's command: attach the remote session, embedding
/// helm's current `SSH_AUTH_SOCK` (if any) so the spawned pane's child ssh can
/// authenticate — it does not inherit helm's env. Pure for testability.
fn viewport_command(sock: Option<&str>, target: &str) -> String {
    match sock {
        Some(s) if !s.is_empty() => format!(
            "SSH_AUTH_SOCK={} helm shell open {}",
            tmux::shell_quote(s),
            tmux::shell_quote(target)
        ),
        _ => format!("helm shell open {}", tmux::shell_quote(target)),
    }
}

/// Render `list-panes` output (`pane_id<TAB>@helm_label<TAB>@helm_viewport`)
/// into `name<TAB>kind<TAB>pane_id` rows, skipping untagged panes. Pure.
fn render_pane_list(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let mut it = line.splitn(4, '\t');
        let id = it.next().unwrap_or("");
        let label = it.next().unwrap_or("");
        let view = it.next().unwrap_or("");
        let bg = it.next().unwrap_or("");
        if !label.is_empty() {
            out.push(format!("{label}\tdrivable\t{id}"));
        } else if !view.is_empty() {
            out.push(format!("{view}\tviewport\t{id}"));
        } else if !bg.is_empty() {
            out.push(format!("{bg}\tbackground\t{id}"));
        }
    }
    out
}

#[cfg(test)]
#[path = "pane_tests.rs"]
mod tests;
