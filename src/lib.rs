//! helm library crate. Every module lives here (`pub mod`) so integration
//! tests and the `fuzz/` crate can reach the command builders and pure logic —
//! command injection is helm's #1 risk, and the builders (`tmux::shell_quote`,
//! `tmux::parse_target`, the argv assemblers) are unreachable from a bin-only
//! crate. The binary (`src/main.rs`) is a thin shim that calls [`run`].

pub mod activity;
pub mod args;
pub mod cli;
pub mod config;
pub mod history;
pub mod inventory;
pub mod mosh;
pub mod opts;
pub mod pane;
pub mod readcursor;
pub mod runcmd;
pub mod shell;
pub mod ssh;
pub mod tmux;
pub mod vultr;
pub mod watch;

use std::process::Command;

use crate::config::Config;
use crate::history::{HistoryStore, LineKind, LineRecord, RunSource};
use crate::ssh::{RunEvent, RunHandle, spawn_remote};

/// Parse argv, route the verb, and run it. The thin binary (`src/main.rs`)
/// calls this and returns its exit code. clap owns argv → verb routing,
/// `--help`/`--version`, and usage errors; each verb keeps its own tested tail
/// parser (see `src/args.rs`).
pub fn run() -> std::process::ExitCode {
    use clap::Parser;
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
        args::Cmd::Pane { args } => pane::run_cli(&args),
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

    if alias == tmux::LOCAL_ALIAS {
        // The local alias must NOT fork in-process: helm is often invoked
        // from inside a sandboxed caller (an agent harness fence), and an
        // in-process `sh -c` inherits that sandbox — defeating the escape
        // hatch. Route through the local tmux server instead (same engine as
        // `helm shell run`): the server lives outside the caller's fence, so
        // the forked shell escapes it.
        return exec_local(&cmd);
    }

    // Direct ssh spawn — no daemon; the transcript is streamed, recorded to
    // history under the `agent` source, and logged (same path as `helm run`).
    stream_and_record("helm exec", RunSource::Agent, &alias, &cmd)
}

/// Session target for `helm exec local`: a dedicated session on the local
/// tmux server (`helm-exec` after `parse_target`), kept apart from the
/// operator's `helm shell open local:*` sessions.
pub const EXEC_LOCAL_TARGET: &str = "local:exec";

/// `helm exec local` — run the command in the `helm-exec` session on the
/// local tmux server via the `runcmd` sentinel engine (shell-run semantics:
/// 30s poll timeout, busy/gone detection). Requires a *running* server: when
/// it is unreachable (no server, or the socket is masked by the caller's
/// sandbox) this fails loudly and never falls back to an in-process fork —
/// a fallback would silently reintroduce the sandbox-inheritance bug — and
/// never auto-starts a server, which would itself be born inside the fence.
fn exec_local(cmd: &str) -> std::process::ExitCode {
    if cmd.contains('\n') {
        // The sentinel wrapper needs a single line (same limit as shell run).
        eprintln!("helm exec local: command must be a single line (no newlines)");
        return std::process::ExitCode::from(2);
    }
    let probe = tmux::runner_cmd(tmux::LOCAL_ALIAS, &exec_local_probe_script()).status();
    if !probe.map(|s| s.success()).unwrap_or(false) {
        eprintln!(
            "helm exec local: no reachable local tmux server. exec local runs the command \
             through the tmux server so it escapes a sandboxed caller, and never falls back \
             to an in-process fork. Start tmux (outside any sandbox) and retry, or target an \
             existing pane with `helm pane run`."
        );
        return std::process::ExitCode::FAILURE;
    }
    let (alias, session) = tmux::parse_target(EXEC_LOCAL_TARGET);
    match runcmd::run_command(EXEC_LOCAL_TARGET, cmd, runcmd::DEFAULT_RUN_TIMEOUT_SECS) {
        Ok(outcome) => {
            // Same per-state narration as `helm shell run`, exec-flavored.
            let logged = if outcome.busy {
                eprintln!(
                    "helm: exec session busy (a previous `helm exec local` command is still \
                     running). Poll with `helm shell read {EXEC_LOCAL_TARGET}`."
                );
                "busy".to_string()
            } else if outcome.gone {
                eprintln!(
                    "helm: the command terminated the exec session — no exit code. \
                     The next `helm exec local` recreates it."
                );
                "gone".to_string()
            } else {
                if !outcome.output.is_empty() {
                    println!("{}", outcome.output);
                }
                match outcome.exit {
                    Some(code) => eprintln!("exit: {code}"),
                    None => eprintln!(
                        "helm: command still running after {}s (timeout). Poll with \
                         `helm shell read {EXEC_LOCAL_TARGET}`.",
                        runcmd::DEFAULT_RUN_TIMEOUT_SECS
                    ),
                }
                activity::preview(&outcome.output)
            };
            log_action(
                activity::ActivityKind::Exec,
                &alias,
                &session,
                cmd,
                &logged,
                outcome.exit.or(if outcome.busy || outcome.gone {
                    Some(1)
                } else {
                    None
                }),
            );
            std::process::ExitCode::from(opts::run_exit_byte(
                outcome.busy,
                outcome.gone,
                outcome.exit,
            ))
        }
        Err(e) => {
            log_action(
                activity::ActivityKind::Exec,
                &alias,
                &session,
                cmd,
                "",
                Some(1),
            );
            eprintln!("helm: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Script for the exec-local reachability probe: `tmux info` connects to the
/// server and fails when there is none (or the socket is unreachable) without
/// ever starting one. Pure builder so the shape is unit-tested.
fn exec_local_probe_script() -> String {
    format!("{} info >/dev/null 2>&1", tmux::tmux_prefix())
}

/// Spawn `cmd` on `alias`, stream its output live, and record the completed run
/// to history under `source` plus the activity log — returning the process exit
/// code. Shared by `helm exec` (agent) and `helm run` (operator); `surface`
/// names the verb in the password-prompt message.
pub(crate) fn stream_and_record(
    surface: &str,
    source: RunSource,
    alias: &str,
    cmd: &str,
) -> std::process::ExitCode {
    let started_at = std::time::Instant::now();
    let started_at_unix = activity::now_unix() as i64;
    let mut lines: Vec<LineRecord> = vec![LineRecord {
        kind: LineKind::System,
        line: format!("$ ssh {alias} '{cmd}'"),
    }];

    let exit = match spawn_remote(alias, cmd) {
        Ok(handle) => {
            let code = drain_run(surface, handle, &mut lines);
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

    persist_run(
        source,
        alias,
        cmd,
        started_at_unix,
        exit,
        started_at.elapsed(),
        &lines,
    );
    log_action(activity::ActivityKind::Exec, alias, "", cmd, "", Some(exit));
    std::process::ExitCode::from(clamp_exit(exit))
}

/// Map a raw exit integer (possibly negative from a signal death, which the
/// wait thread reports as -1) into a process exit byte: out-of-range → 130, the
/// conventional "command died abnormally" code. Wrapping silently to 255 via
/// `as u8` would collide with the legitimate "argument list too long" exit on
/// most shells; 130 ("Ctrl-C") is the closest match to the signal death lost by
/// serializing through u8.
pub(crate) fn clamp_exit(exit: i32) -> u8 {
    if (0..=255).contains(&exit) {
        exit as u8
    } else {
        130
    }
}

/// Block on a remote command's event stream, printing output live and
/// appending every line to `lines` for history persistence. Shared by `helm
/// exec` and `helm run` (`surface` names the verb): neither has a TTY or modal
/// to answer an interactive password prompt, so on `NeedPassword` it closes
/// stdin — the remote `doas`/`sudo` then gets EOF and fails fast instead of
/// blocking this loop forever on a PTY read no one will satisfy.
fn drain_run(surface: &str, mut handle: RunHandle, lines: &mut Vec<LineRecord>) -> i32 {
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
                    "helm: password prompt detected — `{surface}` can't answer it; \
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

/// Best-effort: append one completed run to the history DB under `source`. A
/// history failure must never change the command's exit code, so errors are
/// reported to stderr and otherwise swallowed.
/// True when a history-db open failure is *environmental* — helm was invoked
/// from inside a read-only sandbox fence (EROFS on the state dir, or sqlite's
/// "attempt to write a readonly database" from the migrate step) or a
/// permission mask. History is best-effort, and one warning per CLI call
/// pollutes every captured output an agent reads; these causes are silenced
/// while every other failure (disk full, corruption, bad path) stays loud.
/// Mirrors `activity::environmental_write_error` for the sibling log.
fn environmental_db_error(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return matches!(
                io.kind(),
                std::io::ErrorKind::ReadOnlyFilesystem | std::io::ErrorKind::PermissionDenied
            );
        }
        if let Some(rusqlite::Error::SqliteFailure(f, _)) = cause.downcast_ref::<rusqlite::Error>()
        {
            return matches!(
                f.code,
                rusqlite::ErrorCode::ReadOnly | rusqlite::ErrorCode::PermissionDenied
            );
        }
        false
    })
}

fn persist_run(
    source: RunSource,
    alias: &str,
    cmd: &str,
    started_at_unix: i64,
    exit: i32,
    elapsed: std::time::Duration,
    lines: &[LineRecord],
) {
    let mut store = match HistoryStore::open_default() {
        Ok(s) => s,
        Err(e) if environmental_db_error(&e) => return,
        Err(e) => {
            eprintln!("helm: warning — could not open history db: {e}");
            return;
        }
    };
    let duration_ms = i64::try_from(elapsed.as_millis()).ok();
    if let Err(e) = store.insert_run(
        source,
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
    let record = activity::ActivityRecord::build(kind, alias, session, cmd, output_preview, exit);
    activity::append(&record);
}

/// Load ~/.ssh/config according to the cfg's `[ssh_config]` section,
/// merging the parsed hosts into `cfg` and returning the unmerged list
/// so the agent check can iterate IdentityFiles. Used by `run_auth_cli`.
pub(crate) fn load_ssh_hosts_for(cfg: &mut Config) -> Vec<crate::ssh::sshconfig::SshHost> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_exit_maps_out_of_range_to_130() {
        assert_eq!(clamp_exit(0), 0);
        assert_eq!(clamp_exit(1), 1);
        assert_eq!(clamp_exit(255), 255);
        assert_eq!(clamp_exit(-1), 130);
        assert_eq!(clamp_exit(256), 130);
    }

    #[test]
    fn exec_local_target_names_dedicated_session() {
        // exec local must land in its own session, not the operator's `helm`.
        let (alias, session) = tmux::parse_target(EXEC_LOCAL_TARGET);
        assert_eq!(alias, tmux::LOCAL_ALIAS);
        assert_eq!(session, "helm-exec");
    }

    #[test]
    fn exec_local_probe_connects_without_starting_a_server() {
        // `tmux info` fails when no server is reachable; anything that can
        // CREATE a server (new-session, start-server) would be born inside a
        // sandboxed caller's fence — the exact bug the probe guards against.
        let script = exec_local_probe_script();
        assert!(script.contains(" info"), "{script}");
        assert!(!script.contains("new-session"), "{script}");
        assert!(!script.contains("start-server"), "{script}");
    }

    #[test]
    fn environmental_db_error_matches_fence_causes_only() {
        let erofs = anyhow::Error::from(std::io::Error::from_raw_os_error(30)) // EROFS
            .context("create history dir /x");
        assert!(environmental_db_error(&erofs));
        let eacces = anyhow::Error::from(std::io::Error::from_raw_os_error(13)); // EACCES
        assert!(environmental_db_error(&eacces));
        let readonly_db = anyhow::Error::from(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_READONLY),
            Some("attempt to write a readonly database".into()),
        ));
        assert!(environmental_db_error(&readonly_db));
        // Anything else stays loud.
        let enospc = anyhow::Error::from(std::io::Error::from_raw_os_error(28)); // ENOSPC
        assert!(!environmental_db_error(&enospc));
        assert!(!environmental_db_error(&anyhow::anyhow!("corrupt db")));
    }
}
