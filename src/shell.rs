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

fn shell_run(args: &[String]) -> std::process::ExitCode {
    // helm shell run <target> [--timeout SECS] <cmd...>
    if args.is_empty() {
        eprintln!("usage: helm shell run <target> [--timeout SECS] <cmd...>");
        return std::process::ExitCode::from(2);
    }
    let target = &args[0];
    let mut timeout = tmux::DEFAULT_RUN_TIMEOUT_SECS;
    let mut cmd_parts: Vec<String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--timeout" {
            let Some(v) = args.get(i + 1) else {
                eprintln!("helm shell run: --timeout requires a value (seconds)");
                return std::process::ExitCode::from(2);
            };
            match v.parse::<u32>() {
                Ok(s) if s > 0 => timeout = s,
                _ => {
                    eprintln!("helm shell run: --timeout requires a positive integer");
                    return std::process::ExitCode::from(2);
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
        eprintln!("usage: helm shell run <target> [--timeout SECS] <cmd...>");
        return std::process::ExitCode::from(2);
    }
    // A newline would detach the sentinel printf from the command (the shell
    // submits the first line on its own), so `$?` would report the wrong
    // exit and the poll could hang. Reject up front.
    if cmd.contains('\n') {
        eprintln!("helm shell run: command must be a single line (no newlines)");
        return std::process::ExitCode::from(2);
    }
    let (alias, _) = tmux::parse_target(target);
    let session_label = tmux_label_from_target(target);
    match tmux::run_command(target, &cmd, timeout) {
        Ok(outcome) if outcome.busy => {
            log_action(
                activity::ActivityKind::ShellRun,
                &alias,
                &session_label,
                &cmd,
                "busy",
                Some(1),
            );
            eprintln!(
                "helm: shell busy (not at a prompt — pager/editor/long command). \
                 Use `read`/`send`, or `key` for a TUI."
            );
            std::process::ExitCode::FAILURE
        }
        Ok(outcome) if outcome.gone => {
            log_action(
                activity::ActivityKind::ShellRun,
                &alias,
                &session_label,
                &cmd,
                "gone",
                Some(1),
            );
            eprintln!(
                "helm: the command terminated the shell session ({target} is gone). \
                 Reopen with `helm shell open -d {target}`."
            );
            std::process::ExitCode::FAILURE
        }
        Ok(outcome) => {
            if !outcome.output.is_empty() {
                println!("{}", outcome.output);
            }
            match outcome.exit {
                Some(code) => {
                    eprintln!("exit: {code}");
                    log_action(
                        activity::ActivityKind::ShellRun,
                        &alias,
                        &session_label,
                        &cmd,
                        &activity::preview(&outcome.output),
                        Some(code),
                    );
                    // Mirror the command's own exit, clamped to the byte
                    // range a process exit code occupies (`$?` is 0..=255).
                    std::process::ExitCode::from(code.clamp(0, 255) as u8)
                }
                None => {
                    eprintln!(
                        "helm: command still running after {timeout}s (timeout). \
                         Poll with `helm shell read {target}`."
                    );
                    log_action(
                        activity::ActivityKind::ShellRun,
                        &alias,
                        &session_label,
                        &cmd,
                        &activity::preview(&outcome.output),
                        None,
                    );
                    // 124 is the conventional "timed out" exit (GNU timeout).
                    std::process::ExitCode::from(124)
                }
            }
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

fn shell_read(args: &[String]) -> std::process::ExitCode {
    // helm shell read <target> [-n LINES] [--raw]
    let mut target: Option<&str> = None;
    let mut lines = tmux::DEFAULT_CAPTURE_LINES;
    let mut raw = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" => {
                let Some(v) = args.get(i + 1) else {
                    eprintln!("helm shell read: -n requires a positive integer");
                    return std::process::ExitCode::from(2);
                };
                match v.parse::<u32>() {
                    Ok(parsed) => lines = parsed,
                    Err(_) => {
                        eprintln!("helm shell read: -n requires a positive integer");
                        return std::process::ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--raw" => {
                raw = true;
                i += 1;
            }
            other => {
                if target.is_some() {
                    eprintln!("usage: helm shell read <target> [-n LINES] [--raw]");
                    return std::process::ExitCode::from(2);
                }
                target = Some(other);
                i += 1;
            }
        }
    }
    let Some(target) = target else {
        eprintln!("usage: helm shell read <target> [-n LINES] [--raw]");
        return std::process::ExitCode::from(2);
    };
    let (alias, _) = tmux::parse_target(target);
    let session_label = tmux_label_from_target(target);
    if let Err(e) = tmux::ensure_session(target) {
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
    match tmux::capture(target, lines) {
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
