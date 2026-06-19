mod activity;
mod cli;
mod config;
mod engine;
mod history;
mod inventory;
mod ipc;
mod money;
mod mosh;
mod ssh;
mod tmux;
mod vultr;

use std::io;
use std::process::Command;
use std::time::Duration;

use anyhow::Result;

use crate::config::Config;
use crate::ipc::protocol::Request as IpcRequest;

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let Some(sub) = argv.get(1) else {
        // No subcommand — print usage and exit. (The TUI used to launch
        // here; helm is CLI-only now.)
        print_help();
        return std::process::ExitCode::SUCCESS;
    };
    match sub.as_str() {
        "exec" => run_exec_cli(&argv[2..]),
        "shell" => run_shell_cli(&argv[2..]),
        "open" => {
            // `helm open <target>` attaches a persistent shell to any ssh
            // alias / host / IP — the explicit successor to the old bare
            // `helm <target>` sugar. Forward the tail so `<alias>:<label>`
            // works.
            let mut shell_args = vec!["open".to_string()];
            shell_args.extend(argv[2..].iter().cloned());
            run_shell_cli(&shell_args)
        }
        "auth" => run_auth_cli(&argv[2..]),
        "daemon" => run_daemon_cli(&argv[2..]),
        "--help" | "-h" | "help" => {
            print_help();
            std::process::ExitCode::SUCCESS
        }
        other if !other.starts_with('-') => {
            // Read verb (`ls`, `svc`, `health`, …)? Handle and exit. These
            // are reserved words; an ssh alias colliding with one is
            // shadowed — attach explicitly with `helm open <alias>`.
            if let Some(exit) = cli::dispatch(other, &argv[2..]) {
                return exit;
            }
            eprintln!("helm: unknown command `{other}` (see `helm help`)");
            std::process::ExitCode::from(2)
        }
        other => {
            eprintln!("helm: unknown flag `{other}` (see `helm help`)");
            std::process::ExitCode::from(2)
        }
    }
}

fn print_help() {
    eprintln!(
        "helm — fleet manager + remote command bridge

usage:
  helm open <target>            attach a persistent shell to any ssh alias,
                                host, or IP (e.g. `helm open web`,
                                `helm open web:deploy`)
  helm exec <alias> <cmd...>    run a one-shot command on a host through the
                                running daemon over its control socket
  helm shell <subcommand>       drive a persistent tmux-backed shell session
                                per VPS (see `helm shell help`)

  read commands (add --json for machine output):
  helm ls                       list configured + ssh_config hosts
  helm show <host>              one host's detail + linked businesses
  helm svc <host>               service inventory (rcctl/systemctl/launchctl)
  helm ps <host> [-n N]         top processes by CPU
  helm ports <host>             listening sockets
  helm health                   per-business HTTPS reachability + TLS expiry
  helm dns                      per-business A/AAAA/MX/CAA vs expected IP
  helm vultr                    Vultr instances + monthly cost
  helm money                    Stripe + Mercury balances
  helm logs <host> [key] [-f]   list or tail a host's logs
  helm history [-n N]           recent command history
  helm activity [-n N]          recent agent audit log

  mutations (operator-only; refuse without --yes):
  helm vultr reboot|halt|start|snapshot <id> --yes
  helm run <key> <host> --yes   run a [[shortcuts]] command on a host

  helm auth [--load]            verify ssh-agent has every key helm hosts
                                depend on; exit 0/non-zero (see `helm auth help`)
  helm daemon [start|stop|status]
                                run / manage the headless control daemon
                                (see `helm daemon help`)
  helm help                     this help"
    );
}

fn print_daemon_help() {
    eprintln!(
        "helm daemon — headless control daemon

The daemon binds helm's control socket and services `helm exec` requests
so external operators (e.g. AI agents) can drive remote commands. Start
it before using `helm exec`; only one daemon owns the socket at a time.

usage:
  helm daemon            run in the foreground (logs to stderr); exits
                         on SIGINT, SIGTERM, or a Shutdown IPC request
  helm daemon start      spawn a detached daemon and exit once the
                         socket is reachable
  helm daemon stop       ask a running daemon to exit cleanly
  helm daemon status     exit 0 if a daemon is reachable; 1
                         otherwise. Prints the responder's version.
  helm daemon help       this help"
    );
}

fn print_auth_help() {
    eprintln!(
        "helm auth — verify (and optionally load) ssh-agent keys

Reads your `config.toml` and `~/.ssh/config`, computes the set of
IdentityFile fingerprints helm hosts depend on, and checks whether
ssh-agent already holds them. Designed to be wired into login shells or
sudo/doas wrappers via its exit code:

  exit 0  agent reachable and every key is loaded
  exit 1  one of: agent unreachable, key missing, ssh-add not on PATH
  exit 2  argument error

usage:
  helm auth              one-shot check; exit code reflects status
  helm auth --load       same as bare `helm auth`, plus: when keys are
                         missing, exec `ssh-add <path>` for each (prompts
                         for the passphrase) and re-check
  helm auth help         this help"
    );
}

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
  helm shell read <target> [-n LINES] capture the active pane's
                                      scrollback (default 1000); creates
                                      if missing
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

fn run_shell_cli(args: &[String]) -> std::process::ExitCode {
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

fn shell_read(args: &[String]) -> std::process::ExitCode {
    // helm shell read <target> [-n LINES]
    let (target, lines) = match args {
        [t] => (t.as_str(), tmux::DEFAULT_CAPTURE_LINES),
        [t, flag, n] if flag == "-n" => match n.parse::<u32>() {
            Ok(parsed) => (t.as_str(), parsed),
            Err(_) => {
                eprintln!("helm shell read: -n requires a positive integer");
                return std::process::ExitCode::from(2);
            }
        },
        _ => {
            eprintln!("usage: helm shell read <target> [-n LINES]");
            return std::process::ExitCode::from(2);
        }
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
            log_action(
                activity::ActivityKind::ShellRead,
                &alias,
                &session_label,
                &format!("-n {lines}"),
                &activity::preview(&s),
                Some(0),
            );
            print!("{s}");
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

fn run_exec_cli(args: &[String]) -> std::process::ExitCode {
    if args.len() < 2 {
        eprintln!("usage: helm exec <alias> <cmd...>");
        return std::process::ExitCode::from(2);
    }
    let alias = args[0].clone();
    let cmd = args[1..].join(" ");
    let exit = ipc::client::run_capturing(&IpcRequest::Exec {
        alias: alias.clone(),
        cmd: cmd.clone(),
    });
    log_action(
        activity::ActivityKind::Exec,
        &alias,
        "",
        &cmd,
        "",
        Some(exit),
    );
    // Map any out-of-range exit (including negatives from signal deaths,
    // which the server reports as -1) to the conventional "command died
    // abnormally" code 130. Wrapping silently to 255 via `as u8` would
    // collide with the legitimate "argument list too long" exit on most
    // shells; 130 ("Ctrl-C") is the closest semantic match to the
    // underlying signal death we lose by serializing through u8.
    let c: u8 = if (0..=255).contains(&exit) {
        exit as u8
    } else {
        130
    };
    std::process::ExitCode::from(c)
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

/// Construct + append an activity record from the CLI side. Best-effort;
/// failures are logged to stderr inside `activity::append` and never
/// propagate.
fn log_action(
    kind: activity::ActivityKind,
    alias: &str,
    session: &str,
    cmd: &str,
    output_preview: &str,
    exit: Option<i32>,
) {
    let record = activity::ActivityRecord {
        ts_unix: activity::now_unix(),
        pid: std::process::id(),
        ppid: activity::ppid(),
        kind,
        alias: alias.to_string(),
        session: session.to_string(),
        cmd: cmd.to_string(),
        output_preview: output_preview.to_string(),
        has_privilege_escalation: activity::has_privilege_escalation(cmd),
        exit,
    };
    activity::append(&record);
}

/// Load ~/.ssh/config according to the cfg's `[ssh_config]` section,
/// merging the parsed hosts into `cfg` and returning the unmerged list
/// so the agent check can iterate IdentityFiles. Shared by
/// `run_auth_cli` and the daemon startup.
fn load_ssh_hosts_for(cfg: &mut Config) -> Vec<crate::ssh::sshconfig::SshHost> {
    if !cfg.ssh_config.enabled {
        return Vec::new();
    }
    let path = cfg
        .ssh_config
        .path
        .clone()
        .or_else(crate::ssh::sshconfig::default_config_path);
    let Some(p) = path else { return Vec::new() };
    if !p.exists() {
        return Vec::new();
    }
    match crate::ssh::sshconfig::load_from(&p) {
        Ok(hs) => {
            let ssh_hosts = hs.clone();
            cfg.merge_ssh_hosts(hs);
            ssh_hosts
        }
        Err(e) => {
            tracing::warn!("ssh config {}: {}", p.display(), e);
            Vec::new()
        }
    }
}

fn run_auth_cli(args: &[String]) -> std::process::ExitCode {
    let mut load = false;
    for a in args {
        match a.as_str() {
            "--load" => load = true,
            "help" | "--help" | "-h" => {
                print_auth_help();
                return std::process::ExitCode::SUCCESS;
            }
            other => {
                eprintln!("helm auth: unknown arg `{other}`");
                print_auth_help();
                return std::process::ExitCode::from(2);
            }
        }
    }

    let mut cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("helm auth: config load failed: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let ssh_hosts = load_ssh_hosts_for(&mut cfg);

    use crate::ssh::agent::AgentStatus;
    let status = crate::ssh::agent::check(&ssh_hosts);
    match &status {
        AgentStatus::Ok => {
            eprintln!(
                "helm auth: ssh-agent OK — {} host(s) verified",
                ssh_hosts.len()
            );
            std::process::ExitCode::SUCCESS
        }
        AgentStatus::MissingKeys(missing) if load => {
            // Interactive: exec `ssh-add <path>` per missing key so the
            // user can type the passphrase. We inherit stdio so the
            // prompt lands in the operator's terminal.
            //
            // Failure handling: keep going on ssh-add error (so a typo'd
            // passphrase on key #2 doesn't hide that keys #3..N were
            // never attempted). Track loaded count for a final summary.
            let total = missing.len();
            let mut loaded_ok: Vec<&std::path::Path> = Vec::new();
            let mut failed: Vec<&std::path::Path> = Vec::new();
            for m in missing {
                eprintln!(
                    "helm auth: loading {} (used by {})…",
                    m.identity_file.display(),
                    m.used_by.join(", ")
                );
                let st = Command::new("ssh-add")
                    .arg(&m.identity_file)
                    .stdin(std::process::Stdio::inherit())
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit())
                    .status();
                if matches!(st, std::result::Result::Ok(s) if s.success()) {
                    loaded_ok.push(&m.identity_file);
                } else {
                    failed.push(&m.identity_file);
                }
            }
            if !failed.is_empty() {
                eprintln!(
                    "helm auth: loaded {}/{} keys ({} failed)",
                    loaded_ok.len(),
                    total,
                    failed.len()
                );
                for p in &failed {
                    eprintln!("  ✗ {}", p.display());
                }
                return std::process::ExitCode::FAILURE;
            }
            // All ssh-add calls returned 0 — re-check agent state.
            match crate::ssh::agent::check(&ssh_hosts) {
                AgentStatus::Ok => {
                    eprintln!("helm auth: {total} key(s) loaded — agent OK");
                    std::process::ExitCode::SUCCESS
                }
                _ => {
                    eprintln!("helm auth: still missing keys after load");
                    std::process::ExitCode::FAILURE
                }
            }
        }
        _ => {
            if let Some(msg) = crate::ssh::agent::render_blocker(&status, &ssh_hosts) {
                eprintln!("{msg}");
            }
            std::process::ExitCode::FAILURE
        }
    }
}

fn run_daemon_cli(args: &[String]) -> std::process::ExitCode {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "" => run_daemon_foreground_cli(),
        "start" => run_daemon_start_cli(),
        "stop" => run_daemon_stop_cli(),
        "status" => run_daemon_status_cli(),
        "help" | "--help" | "-h" => {
            print_daemon_help();
            std::process::ExitCode::SUCCESS
        }
        other => {
            eprintln!("helm daemon: unknown subcommand `{other}`");
            print_daemon_help();
            std::process::ExitCode::from(2)
        }
    }
}

fn run_daemon_foreground_cli() -> std::process::ExitCode {
    match run_daemon_foreground() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("helm daemon: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Headless engine + IPC server loop: loads config + binds the IPC server,
/// then drives a small tokio runtime to multiplex signal handling with
/// periodic engine ticks.
fn run_daemon_foreground() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(io::stderr)
        .init();

    let mut cfg = Config::load()?;
    tmux::set_flags(cfg.tmux_flags());
    let ssh_hosts = load_ssh_hosts_for(&mut cfg);
    let agent_status = crate::ssh::agent::check(&ssh_hosts);
    if let Some(msg) = crate::ssh::agent::render_blocker(&agent_status, &ssh_hosts) {
        eprintln!("{msg}");
        std::process::exit(1);
    }

    let mut engine = crate::engine::Engine::new();
    match history::HistoryStore::open_default() {
        Ok(store) => engine.attach_history(store, 100),
        Err(e) => eprintln!("helm: warning — could not open history db: {e}"),
    }

    let socket = crate::ipc::socket_path();
    let handles = crate::ipc::server::start(socket.clone())
        .map_err(|e| anyhow::anyhow!("bind {} failed: {e}", socket.display()))?;
    eprintln!(
        "helm daemon: control socket at {}",
        handles.guard.socket_path.display()
    );
    engine.attach_jobs_rx(handles.jobs_rx);
    let shutdown_rx = handles.shutdown_rx;
    // Held across the loop so Drop removes the socket file on clean exit.
    let _socket_guard = handles.guard;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigint = signal(SignalKind::interrupt())?;
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    engine.ingest_jobs(None);
                    engine.ingest_agent_events();
                    if shutdown_rx.try_recv().is_ok() {
                        eprintln!("helm daemon: shutdown via IPC");
                        break;
                    }
                }
                _ = sigterm.recv() => {
                    eprintln!("helm daemon: SIGTERM");
                    break;
                }
                _ = sigint.recv() => {
                    eprintln!("helm daemon: SIGINT");
                    break;
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    })?;
    Ok(())
}

fn run_daemon_start_cli() -> std::process::ExitCode {
    match spawn_detached_daemon() {
        Ok(pid) => {
            eprintln!("helm daemon: up (pid {pid})");
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("helm daemon: start failed: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Fork-exec a detached copy of this binary in `daemon` foreground mode,
/// then poll the control socket until it answers Ping (up to 3s). Returns
/// the child PID on success.
fn spawn_detached_daemon() -> Result<u32> {
    let socket = crate::ipc::socket_path();
    if let Some(v) = crate::ipc::client::ping_socket(&socket)? {
        return Err(anyhow::anyhow!(
            "another helm process (v{v}) is already bound to {}",
            socket.display()
        ));
    }
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Detach into a new session so the daemon survives the spawning shell.
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            // setsid returns -1 on failure, but only fails if the caller is
            // already a session leader — harmless for our spawn-fresh case.
            libc::setsid();
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    let pid = child.id();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if crate::ipc::client::ping_socket(&socket)?.is_some() {
            return Ok(pid);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(anyhow::anyhow!(
        "daemon spawned (pid {pid}) but socket {} didn't come up in 3s",
        socket.display()
    ))
}

fn run_daemon_stop_cli() -> std::process::ExitCode {
    let socket = crate::ipc::socket_path();
    if !socket.exists() {
        eprintln!("helm daemon: not running");
        return std::process::ExitCode::SUCCESS;
    }
    match crate::ipc::client::shutdown_socket(&socket, Duration::from_secs(3)) {
        Ok(true) => {
            eprintln!("helm daemon: stopped");
            std::process::ExitCode::SUCCESS
        }
        Ok(false) => {
            eprintln!("helm daemon: socket still present after 3s");
            std::process::ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("helm daemon: stop failed: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run_daemon_status_cli() -> std::process::ExitCode {
    let socket = crate::ipc::socket_path();
    match crate::ipc::client::ping_socket(&socket) {
        Ok(Some(v)) => {
            println!("running (helm v{v}) at {}", socket.display());
            std::process::ExitCode::SUCCESS
        }
        Ok(None) => {
            println!("not running");
            std::process::ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("helm daemon: status check failed: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
