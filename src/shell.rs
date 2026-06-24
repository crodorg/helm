//! `helm shell` subcommands: persistent tmux sessions (local or over ssh)
//! plus the agent-facing `run`/`key`/`read` verbs. Split out of `main.rs` to
//! keep both files under the per-file size cap. Handlers call back into
//! `crate::log_action` for the shared activity log.

use crate::activity;
use crate::config::Config;
use crate::log_action;
use crate::tmux;

fn print_shell_help() {
    eprintln!(
        "helm shell — persistent tmux shell sessions on a chosen host

Sessions live on the chosen host. Each `<target>` is `<alias>` (default
session `helm`) or `<alias>:<label>` (session `helm-<label>`). The
reserved alias `local` runs against your own machine instead of ssh —
use it for shells that need interactive doas/sudo password entry or a
separate history from your own terminal. Persistence survives helm
restarts, network drops, and operator-machine reboots for remote
sessions; local sessions survive until your machine reboots or its
tmux server is killed.

usage:
  helm shell open <target>            attach this terminal to the
                                      session (creates if missing)
  helm shell open -d <target>         create the session detached; do
                                      not attach
  helm shell send <target> <text...>  send a line of text (auto-Enter)
                                      to the session's active pane;
                                      creates the session if missing
  helm shell run <target> <cmd...>    send a command, wait for it to
                                      finish, print only its output and
   [--timeout SECS]                   `exit: N`; one ssh round-trip
                                      (default timeout 30s). Single-line,
                                      non-interactive commands only
  helm shell key <target> <key...>    send raw tmux key specs (no Enter):
                                      Up Down C-c Escape F1 … to drive a
                                      TUI (vim, htop, menus) on the host
  helm shell read <target> [-n LINES] capture the active pane's
   [--raw]                            scrollback (default 200; trailing
                                      blank padding stripped unless
                                      --raw); creates if missing
  helm shell list <alias>             list helm-* sessions on the
                                      alias's tmux server (use `local`
                                      for your own machine)
  helm shell close <target>           kill the session

tip: `helm open <target>` is shorthand for `helm shell open <target>`.

Every tmux call helm makes carries the flags from `tmux_flags` in
config.toml (default `[\"-u\"]`, force UTF-8). Set `tmux_flags = []` to
disable, or list your own."
    );
}

pub(crate) fn run_cli(args: &[String]) -> std::process::ExitCode {
    let Some(sub) = args.first() else {
        print_shell_help();
        return std::process::ExitCode::from(2);
    };
    // Every tmux-touching subcommand needs the configured global tmux flags
    // (e.g. `-u`) installed before it shells out. The shell CLI doesn't
    // otherwise read config, so load it here — silently, since the agent
    // calls these repeatedly. `help` touches no tmux, so skip the load.
    if !matches!(sub.as_str(), "help" | "--help" | "-h") {
        let flags = Config::load_silent()
            .map(|c| c.tmux_flags())
            .unwrap_or_else(|e| {
                eprintln!("helm: warning — config load failed ({e}); using default tmux flags");
                Config::default().tmux_flags()
            });
        tmux::set_flags(flags);
    }
    match sub.as_str() {
        "help" | "--help" | "-h" => {
            print_shell_help();
            std::process::ExitCode::SUCCESS
        }
        "open" => shell_open(&args[1..]),
        "send" => shell_send(&args[1..]),
        "run" => shell_run(&args[1..]),
        "key" => shell_key(&args[1..]),
        "read" => shell_read(&args[1..]),
        "list" => shell_list(&args[1..]),
        "close" => shell_close(&args[1..]),
        other => {
            eprintln!("helm shell: unknown subcommand `{other}`");
            print_shell_help();
            std::process::ExitCode::from(2)
        }
    }
}

fn shell_open(args: &[String]) -> std::process::ExitCode {
    // Parse optional -d flag.
    let (detached, target) = match args {
        [flag, t] if flag == "-d" => (true, t.as_str()),
        [t] => (false, t.as_str()),
        _ => {
            eprintln!("usage: helm shell open [-d] <target>");
            return std::process::ExitCode::from(2);
        }
    };
    let (alias, session) = tmux::parse_target(target);
    let session_label = tmux_label_from_target(target);
    if detached {
        let result = tmux::ensure_session(target);
        log_action(
            activity::ActivityKind::ShellOpen,
            &alias,
            &session_label,
            &format!("-d {target}"),
            "",
            match &result {
                Ok(()) => Some(0),
                Err(_) => Some(1),
            },
        );
        if let Err(e) = result {
            eprintln!("helm: {e}");
            return std::process::ExitCode::FAILURE;
        }
        eprintln!("helm: session ready on {alias} — attach with `helm shell open {target}`");
        return std::process::ExitCode::SUCCESS;
    }
    // Attach path replaces the current process via exec(); log BEFORE the
    // exec call since we never return on success. Exit is `None` because
    // we can't observe the attached tmux's eventual exit code.
    log_action(
        activity::ActivityKind::ShellOpen,
        &alias,
        &session_label,
        &format!("attach {target}"),
        "",
        None,
    );
    // Replace current process with `tmux new-session -A -s <session>` —
    // directly when the alias is `local`, otherwise via `ssh -t <alias>`.
    // `-A` makes new-session idempotent: attach if exists, create
    // otherwise. Never returns on success. The remote script is wrapped
    // with `tmux::with_remote_path` so macOS/Homebrew installs of tmux
    // resolve under a non-interactive ssh shell.
    use std::os::unix::process::CommandExt;
    let err = if alias == tmux::LOCAL_ALIAS {
        let mut cmd = std::process::Command::new("tmux");
        cmd.args(tmux::flags());
        cmd.arg("new-session").arg("-A").arg("-s").arg(&session);
        cmd.exec()
    } else {
        let remote = tmux::with_remote_path(&format!(
            "{} new-session -A -s {session}",
            tmux::tmux_prefix()
        ));
        // Pick mosh vs ssh BEFORE exec — exec replaces this process, so a
        // post-failure fallback is impossible. mosh parses no shell syntax
        // itself, so the script rides in `sh -c` after `--`; mosh always
        // allocates a PTY, hence no `-t`.
        match crate::mosh::decide(&alias) {
            crate::mosh::Transport::Mosh => std::process::Command::new("mosh")
                .arg(&alias)
                .arg("--")
                .arg("sh")
                .arg("-c")
                .arg(&remote)
                .exec(),
            crate::mosh::Transport::Ssh => std::process::Command::new("ssh")
                .arg("-t")
                .arg(&alias)
                .arg(&remote)
                .exec(),
        }
    };
    eprintln!("helm: exec attach failed: {err}");
    std::process::ExitCode::FAILURE
}

fn shell_send(args: &[String]) -> std::process::ExitCode {
    if args.len() < 2 {
        eprintln!("usage: helm shell send <target> <text...>");
        return std::process::ExitCode::from(2);
    }
    let target = &args[0];
    let text = args[1..].join(" ");
    let (alias, _) = tmux::parse_target(target);
    let session_label = tmux_label_from_target(target);
    let ensure = tmux::ensure_session(target);
    if let Err(e) = &ensure {
        log_action(
            activity::ActivityKind::ShellSend,
            &alias,
            &session_label,
            &text,
            "",
            Some(1),
        );
        eprintln!("helm: {e}");
        return std::process::ExitCode::FAILURE;
    }
    let send = tmux::send_keys(target, &text);
    log_action(
        activity::ActivityKind::ShellSend,
        &alias,
        &session_label,
        &text,
        "",
        match &send {
            Ok(()) => Some(0),
            Err(_) => Some(1),
        },
    );
    if let Err(e) = send {
        eprintln!("helm: {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

/// Parsed `helm shell run` invocation. Split from `shell_run` so the flag and
/// validation logic is unit-tested without spawning tmux. `Err` carries the
/// usage message to print (all are exit-2 argument errors).
struct RunArgs {
    target: String,
    cmd: String,
    timeout: u32,
}

const RUN_USAGE: &str = "usage: helm shell run <target> [--timeout SECS] <cmd...>";

fn parse_run_args(args: &[String]) -> Result<RunArgs, String> {
    let target = args.first().ok_or_else(|| RUN_USAGE.to_string())?;
    let mut timeout = tmux::DEFAULT_RUN_TIMEOUT_SECS;
    let mut cmd_parts: Vec<String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--timeout" {
            let v = args.get(i + 1).ok_or_else(|| {
                "helm shell run: --timeout requires a value (seconds)".to_string()
            })?;
            match v.parse::<u32>() {
                Ok(s) if s > 0 => timeout = s,
                _ => {
                    return Err("helm shell run: --timeout requires a positive integer".to_string());
                }
            }
            i += 2;
        } else {
            cmd_parts.push(args[i].clone());
            i += 1;
        }
    }
    let cmd = cmd_parts.join(" ");
    if cmd.trim().is_empty() {
        return Err(RUN_USAGE.to_string());
    }
    // A newline would detach the sentinel printf from the command (the shell
    // submits the first line on its own), so `$?` would report the wrong exit
    // and the poll could hang. Reject up front.
    if cmd.contains('\n') {
        return Err("helm shell run: command must be a single line (no newlines)".to_string());
    }
    Ok(RunArgs {
        target: target.clone(),
        cmd,
        timeout,
    })
}

/// The process exit byte `run` returns: the remote command's own exit for a
/// completed run (clamped to the 0..=255 a process code occupies), 1 for a
/// busy or gone session, 124 (the GNU `timeout` convention) for a timeout.
fn run_exit_byte(outcome: &tmux::RunOutcome) -> u8 {
    if outcome.busy || outcome.gone {
        1
    } else {
        match outcome.exit {
            Some(code) => code.clamp(0, 255) as u8,
            None => 124,
        }
    }
}

fn shell_run(args: &[String]) -> std::process::ExitCode {
    let RunArgs {
        target,
        cmd,
        timeout,
    } = match parse_run_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            return std::process::ExitCode::from(2);
        }
    };
    let (alias, _) = tmux::parse_target(&target);
    let session_label = tmux_label_from_target(&target);
    match tmux::run_command(&target, &cmd, timeout) {
        Ok(outcome) => {
            // Per-state narration + audit; the process exit byte is decided
            // by the pure `run_exit_byte` below.
            let logged = if outcome.busy {
                eprintln!(
                    "helm: shell busy (not at a prompt — pager/editor/long command). \
                     Use `read`/`send`, or `key` for a TUI."
                );
                "busy".to_string()
            } else if outcome.gone {
                eprintln!(
                    "helm: the command terminated the shell session ({target} is gone). \
                     Reopen with `helm shell open -d {target}`."
                );
                "gone".to_string()
            } else {
                if !outcome.output.is_empty() {
                    println!("{}", outcome.output);
                }
                match outcome.exit {
                    Some(code) => eprintln!("exit: {code}"),
                    None => eprintln!(
                        "helm: command still running after {timeout}s (timeout). \
                         Poll with `helm shell read {target}`."
                    ),
                }
                activity::preview(&outcome.output)
            };
            log_action(
                activity::ActivityKind::ShellRun,
                &alias,
                &session_label,
                &cmd,
                &logged,
                outcome.exit.or(if outcome.busy || outcome.gone {
                    Some(1)
                } else {
                    None
                }),
            );
            std::process::ExitCode::from(run_exit_byte(&outcome))
        }
        Err(e) => {
            log_action(
                activity::ActivityKind::ShellRun,
                &alias,
                &session_label,
                &cmd,
                "",
                Some(1),
            );
            eprintln!("helm: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn shell_key(args: &[String]) -> std::process::ExitCode {
    if args.len() < 2 {
        eprintln!("usage: helm shell key <target> <key...>  (e.g. Up Up Enter, C-c, Escape)");
        return std::process::ExitCode::from(2);
    }
    let target = &args[0];
    let keys = &args[1..];
    let (alias, _) = tmux::parse_target(target);
    let session_label = tmux_label_from_target(target);
    if let Err(e) = tmux::ensure_session(target) {
        log_action(
            activity::ActivityKind::ShellKey,
            &alias,
            &session_label,
            &keys.join(" "),
            "",
            Some(1),
        );
        eprintln!("helm: {e}");
        return std::process::ExitCode::FAILURE;
    }
    let res = tmux::send_raw_keys(target, keys);
    log_action(
        activity::ActivityKind::ShellKey,
        &alias,
        &session_label,
        &keys.join(" "),
        "",
        match &res {
            Ok(()) => Some(0),
            Err(_) => Some(1),
        },
    );
    if let Err(e) = res {
        eprintln!("helm: {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

/// Parsed `helm shell read` invocation. Split from `shell_read` for the same
/// reason as `RunArgs`. `Err` carries the message to print (all exit-2).
struct ReadArgs {
    target: String,
    lines: u32,
    raw: bool,
}

const READ_USAGE: &str = "usage: helm shell read <target> [-n LINES] [--raw]";

fn parse_read_args(args: &[String]) -> Result<ReadArgs, String> {
    let mut target: Option<&str> = None;
    let mut lines = tmux::DEFAULT_CAPTURE_LINES;
    let mut raw = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "helm shell read: -n requires a positive integer".to_string())?;
                lines = v
                    .parse()
                    .map_err(|_| "helm shell read: -n requires a positive integer".to_string())?;
                i += 2;
            }
            "--raw" => {
                raw = true;
                i += 1;
            }
            other => {
                if target.is_some() {
                    return Err(READ_USAGE.to_string());
                }
                target = Some(other);
                i += 1;
            }
        }
    }
    let target = target.ok_or_else(|| READ_USAGE.to_string())?;
    Ok(ReadArgs {
        target: target.to_string(),
        lines,
        raw,
    })
}

fn shell_read(args: &[String]) -> std::process::ExitCode {
    let ReadArgs { target, lines, raw } = match parse_read_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            return std::process::ExitCode::from(2);
        }
    };
    let (alias, _) = tmux::parse_target(&target);
    let session_label = tmux_label_from_target(&target);
    if let Err(e) = tmux::ensure_session(&target) {
        log_action(
            activity::ActivityKind::ShellRead,
            &alias,
            &session_label,
            &format!("-n {lines}"),
            "",
            Some(1),
        );
        eprintln!("helm: {e}");
        return std::process::ExitCode::FAILURE;
    }
    match tmux::capture(&target, lines) {
        Ok(s) => {
            // Strip the pane's trailing blank padding (a 50-row headless pane
            // is mostly empty) unless the caller asked for the raw capture.
            let s = if raw {
                s
            } else {
                tmux::strip_trailing_blank(&s)
            };
            log_action(
                activity::ActivityKind::ShellRead,
                &alias,
                &session_label,
                &format!("-n {lines}"),
                &activity::preview(&s),
                Some(0),
            );
            if raw {
                print!("{s}");
            } else {
                println!("{s}");
            }
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            log_action(
                activity::ActivityKind::ShellRead,
                &alias,
                &session_label,
                &format!("-n {lines}"),
                "",
                Some(1),
            );
            eprintln!("helm: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn shell_list(args: &[String]) -> std::process::ExitCode {
    let Some(alias) = args.first() else {
        eprintln!("usage: helm shell list <alias>");
        return std::process::ExitCode::from(2);
    };
    match tmux::list(alias) {
        Ok(targets) => {
            let count = targets.len();
            if targets.is_empty() {
                eprintln!("(no helm-* tmux sessions on {alias})");
            } else {
                for t in &targets {
                    println!("{t}");
                }
            }
            log_action(
                activity::ActivityKind::ShellList,
                alias,
                "",
                "",
                &format!("{count} sessions"),
                Some(0),
            );
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            log_action(
                activity::ActivityKind::ShellList,
                alias,
                "",
                "",
                "",
                Some(1),
            );
            eprintln!("helm: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn shell_close(args: &[String]) -> std::process::ExitCode {
    let Some(target) = args.first() else {
        eprintln!("usage: helm shell close <target>");
        return std::process::ExitCode::from(2);
    };
    let (alias, _) = tmux::parse_target(target);
    let session_label = tmux_label_from_target(target);
    match tmux::kill(target) {
        Ok(()) => {
            log_action(
                activity::ActivityKind::ShellClose,
                &alias,
                &session_label,
                target,
                "",
                Some(0),
            );
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            log_action(
                activity::ActivityKind::ShellClose,
                &alias,
                &session_label,
                target,
                "",
                Some(1),
            );
            eprintln!("helm: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Returns just the label after the `:` for an `alias:label` target, or
/// the empty string for a bare `alias` target. Used for the activity
/// log's `session` column.
fn tmux_label_from_target(target: &str) -> String {
    match target.split_once(':') {
        Some((_, label)) => label.to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn run_args_basic_joins_command_words() {
        let r = parse_run_args(&v(&["web", "echo", "hi"])).unwrap();
        assert_eq!(r.target, "web");
        assert_eq!(r.cmd, "echo hi");
        assert_eq!(r.timeout, tmux::DEFAULT_RUN_TIMEOUT_SECS);
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
    fn run_exit_byte_maps_each_state() {
        let mk = |exit, busy, gone| tmux::RunOutcome {
            output: String::new(),
            exit,
            busy,
            gone,
        };
        assert_eq!(run_exit_byte(&mk(Some(0), false, false)), 0);
        assert_eq!(run_exit_byte(&mk(Some(2), false, false)), 2);
        assert_eq!(run_exit_byte(&mk(Some(300), false, false)), 255); // clamped
        assert_eq!(run_exit_byte(&mk(None, false, false)), 124); // timeout
        assert_eq!(run_exit_byte(&mk(None, true, false)), 1); // busy
        assert_eq!(run_exit_byte(&mk(None, false, true)), 1); // gone
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
        assert!(parse_read_args(&v(&[])).is_err());
        // A second positional is ambiguous.
        assert!(parse_read_args(&v(&["web", "extra"])).is_err());
    }
}
