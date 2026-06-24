mod activity;
mod args;
mod cli;
mod config;
mod history;
mod inventory;
mod mosh;
mod shell;
mod ssh;
mod tmux;
mod vultr;

use std::process::Command;

use crate::config::Config;
use crate::history::{HistoryStore, LineKind, LineRecord, RunSource};
use crate::ssh::{RunEvent, RunHandle, spawn_remote};

fn main() -> std::process::ExitCode {
    use clap::Parser;
    // clap owns argv → verb routing, --help/--version, and usage errors;
    // each verb keeps its own tested tail parser (see src/args.rs).
    let cli = args::Cli::parse();
    let Some(cmd) = cli.cmd else {
        // Bare `helm` → print the full (clap-generated) help, exit 0.
        let _ = <args::Cli as clap::CommandFactory>::command().print_long_help();
        println!();
        return std::process::ExitCode::SUCCESS;
    };
    // Read + gated-mutation verbs delegate to the cli module, which keeps its
    // own flag parsing (--json/-n/-f, --yes). clap only routed the verb.
    if let Some((verb, tail)) = cmd.as_read_verb() {
        return cli::dispatch(verb, tail).expect("clap routed a known verb");
    }
    // The remaining verbs carry an arbitrary tail (alias, command words,
    // tmux subcommand) that must not be re-parsed; hand it through verbatim.
    match cmd {
        args::Cmd::Open { args } => {
            // Explicit successor to the old bare `helm <target>` sugar. Forward
            // the tail so `<alias>:<label>` and `-d` still work.
            let mut shell_args = vec!["open".to_string()];
            shell_args.extend(args);
            shell::run_cli(&shell_args)
        }
        args::Cmd::Exec { args } => run_exec_cli(&args),
        args::Cmd::Shell { args } => shell::run_cli(&args),
        args::Cmd::Auth { args } => run_auth_cli(&args),
        _ => unreachable!("read/mutation verbs are handled above"),
    }
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

fn run_exec_cli(args: &[String]) -> std::process::ExitCode {
    if args.len() < 2 {
        eprintln!("usage: helm exec <alias> <cmd...>");
        return std::process::ExitCode::from(2);
    }
    let alias = args[0].clone();
    if alias.is_empty() {
        // The old daemon path rejected an empty alias explicitly; without a
        // guard it would reach `ssh -tt "" cmd` and fail with an opaque
        // "Could not resolve hostname" / exit 255 instead.
        eprintln!("helm exec: <alias> must not be empty");
        return std::process::ExitCode::from(2);
    }
    let cmd = args[1..].join(" ");

    // Direct ssh spawn — no daemon. Output is captured as it streams so the
    // full transcript can be persisted to history.db under the `agent`
    // source, matching what the old daemon engine recorded.
    let started_at = std::time::Instant::now();
    let started_at_unix = activity::now_unix() as i64;
    let mut lines: Vec<LineRecord> = vec![LineRecord {
        kind: LineKind::System,
        line: format!("$ ssh {alias} '{cmd}'"),
    }];

    let exit = match spawn_remote(&alias, &cmd) {
        Ok(handle) => {
            let code = drain_exec(handle, &mut lines);
            lines.push(LineRecord {
                kind: LineKind::System,
                line: format!("exit {code}"),
            });
            code
        }
        Err(e) => {
            eprintln!("helm: spawn failed: {e}");
            lines.push(LineRecord {
                kind: LineKind::System,
                line: format!("spawn failed: {e}"),
            });
            1
        }
    };

    persist_exec_run(
        &alias,
        &cmd,
        started_at_unix,
        exit,
        started_at.elapsed(),
        &lines,
    );
    log_action(
        activity::ActivityKind::Exec,
        &alias,
        "",
        &cmd,
        "",
        Some(exit),
    );

    // Map any out-of-range exit (including negatives from signal deaths,
    // which the wait thread reports as -1) to the conventional "command died
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

/// Block on a remote command's event stream, printing output live and
/// appending every line to `lines` for history persistence. `helm exec` is
/// the agent surface: there is no TTY or modal to answer an interactive
/// password prompt, so on `NeedPassword` it closes stdin — the remote
/// `doas`/`sudo` then gets EOF and fails fast instead of blocking this loop
/// forever on a PTY read no one will satisfy.
fn drain_exec(mut handle: RunHandle, lines: &mut Vec<LineRecord>) -> i32 {
    let mut exit = 1;
    while let Ok(ev) = handle.rx.recv() {
        match ev {
            RunEvent::Out(line) => {
                println!("{line}");
                lines.push(LineRecord {
                    kind: LineKind::Out,
                    line,
                });
            }
            RunEvent::Err(line) => {
                eprintln!("{line}");
                lines.push(LineRecord {
                    kind: LineKind::Err,
                    line,
                });
            }
            RunEvent::Partial(text) => lines.push(LineRecord {
                kind: LineKind::System,
                line: text,
            }),
            RunEvent::NeedPassword => {
                eprintln!(
                    "helm: password prompt detected — `helm exec` can't answer it; \
                     closing input (the command will fail). Use `helm shell open {}` \
                     for interactive auth.",
                    handle.alias
                );
                handle.close_stdin();
                lines.push(LineRecord {
                    kind: LineKind::System,
                    line: "(password prompt — input closed; command will fail)".into(),
                });
            }
            RunEvent::Done(code) => exit = code,
            RunEvent::Error(msg) => {
                eprintln!("helm: {msg}");
                lines.push(LineRecord {
                    kind: LineKind::System,
                    line: format!("error: {msg}"),
                });
                exit = 1;
            }
        }
    }
    exit
}

/// Best-effort: append one completed `helm exec` run to the history DB under
/// the `agent` source. A history failure must never change the command's exit
/// code, so errors are reported to stderr and otherwise swallowed.
fn persist_exec_run(
    alias: &str,
    cmd: &str,
    started_at_unix: i64,
    exit: i32,
    elapsed: std::time::Duration,
    lines: &[LineRecord],
) {
    let mut store = match HistoryStore::open_default() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("helm: warning — could not open history db: {e}");
            return;
        }
    };
    let duration_ms = i64::try_from(elapsed.as_millis()).ok();
    if let Err(e) = store.insert_run(
        RunSource::Agent,
        alias,
        cmd,
        started_at_unix,
        Some(exit),
        duration_ms,
        lines,
    ) {
        eprintln!("helm: warning — history insert failed: {e}");
        return;
    }
    // Cap the DB so it doesn't grow unbounded across runs. The daemon engine
    // used to prune once at startup; with inline exec there is no startup, so
    // prune after each insert (cheap at this row count).
    if let Err(e) = store.prune_to(5000) {
        eprintln!("helm: warning — history prune failed: {e}");
    }
}

/// Construct + append an activity record from the CLI side. Best-effort;
/// failures are logged to stderr inside `activity::append` and never
/// propagate.
pub(crate) fn log_action(
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
/// so the agent check can iterate IdentityFiles. Used by `run_auth_cli`.
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
            eprintln!("helm: warning — ssh config {}: {}", p.display(), e);
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

// Daemon / IPC removed in the Phase 4 teardown: `helm exec` now spawns ssh
// directly (see `run_exec_cli`) and writes history + activity inline, so the
// headless control socket is gone.
