//! Command-line surface — the clap-derive model for `helm <verb> …`.
//!
//! clap owns argv → verb routing, `--help`/`--version`, and usage errors.
//! Every verb captures its raw tail as `Vec<String>`; the handlers in `main`
//! (and `cli::dispatch`) keep their existing, unit-tested parsers, so the
//! agent-facing parsing of `helm exec`, `helm shell`, and the read verbs is
//! byte-for-byte unchanged. This is the "spine-only" migration: clap is the
//! dispatcher, not a re-parse of every flag. `trailing_var_arg` +
//! `allow_hyphen_values` on each tail let a verb carry its own flags
//! (`--json`, `-n`, `--yes`) and arbitrary remote command words verbatim.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "helm",
    version,
    about = "helm — fleet manager + remote command bridge",
    long_about = "helm — fleet manager + remote command bridge.\n\n\
        Read verbs (ls, show, svc, ps, ports, vultr, logs, history, activity) \
        accept --json for machine output; stdout carries the payload, stderr \
        the diagnostics. Mutations (vultr reboot|halt|start|snapshot, run) are \
        operator-only and refuse to fire without --yes."
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Attach a persistent shell to an ssh alias, host, or IP
    Open {
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "TARGET"
        )]
        args: Vec<String>,
    },
    /// Run a one-shot command on a host over ssh (streamed + recorded)
    Exec {
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "ALIAS CMD"
        )]
        args: Vec<String>,
    },
    /// Drive a persistent tmux-backed shell session (open|send|read|list|close)
    Shell {
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "SUBCOMMAND"
        )]
        args: Vec<String>,
    },
    /// Drive panes in helm's own tmux window (open|view|send|key|read|close|list)
    Pane {
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "SUBCOMMAND"
        )]
        args: Vec<String>,
    },

    /// List configured + ssh_config hosts
    Ls {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show one host's detail + linked businesses
    Show {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Service inventory (rcctl/systemctl/launchctl)
    #[command(alias = "services")]
    Svc {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Top processes by CPU
    Ps {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Listening sockets
    Ports {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Vultr instances + monthly cost (reboot|halt|start|snapshot <id> --yes)
    Vultr {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// List or tail a host's logs
    #[command(alias = "log")]
    Logs {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Recent command history (history <id> shows one run's transcript)
    History {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Recent agent audit log
    Activity {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run a [[shortcuts]] command on a host (operator-only; needs --yes)
    Run {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Verify ssh-agent holds every key helm hosts depend on
    Auth {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

impl Cmd {
    /// For the read/mutation verbs that delegate to `cli::dispatch`, return
    /// the canonical verb string + its raw tail. `None` for the verbs `main`
    /// handles itself (open/exec/shell/auth).
    pub fn as_read_verb(&self) -> Option<(&'static str, &[String])> {
        let pair = match self {
            Cmd::Ls { args } => ("ls", args),
            Cmd::Show { args } => ("show", args),
            Cmd::Svc { args } => ("svc", args),
            Cmd::Ps { args } => ("ps", args),
            Cmd::Ports { args } => ("ports", args),
            Cmd::Vultr { args } => ("vultr", args),
            Cmd::Logs { args } => ("logs", args),
            Cmd::History { args } => ("history", args),
            Cmd::Activity { args } => ("activity", args),
            Cmd::Run { args } => ("run", args),
            _ => return None,
        };
        Some((pair.0, pair.1.as_slice()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// clap's own consistency check — catches conflicting short flags,
    /// duplicate names, or bad arg configs at test time, not runtime.
    #[test]
    fn command_definition_is_valid() {
        Cli::command().debug_assert();
    }

    fn parse(argv: &[&str]) -> Cli {
        Cli::try_parse_from(argv).expect("should parse")
    }

    #[test]
    fn bare_helm_has_no_subcommand() {
        assert!(parse(&["helm"]).cmd.is_none());
    }

    #[test]
    fn read_verb_routes_with_raw_tail() {
        let cli = parse(&["helm", "ps", "web", "-n", "5", "--json"]);
        let cmd = cli.cmd.unwrap();
        let (verb, tail) = cmd.as_read_verb().unwrap();
        assert_eq!(verb, "ps");
        assert_eq!(tail, &["web", "-n", "5", "--json"]);
    }

    #[test]
    fn svc_and_logs_aliases_resolve_to_canonical_verb() {
        let svc = parse(&["helm", "services", "web"]);
        assert_eq!(svc.cmd.unwrap().as_read_verb().unwrap().0, "svc");
        let logs = parse(&["helm", "log", "web"]);
        assert_eq!(logs.cmd.unwrap().as_read_verb().unwrap().0, "logs");
    }

    #[test]
    fn exec_keeps_command_flags_in_the_tail() {
        // The remote command's own flags must reach the tail, not clap.
        let cli = parse(&["helm", "exec", "web", "ls", "-la", "--color"]);
        match cli.cmd.unwrap() {
            Cmd::Exec { args } => assert_eq!(args, &["web", "ls", "-la", "--color"]),
            other => panic!("expected Exec, got {other:?}"),
        }
        // exec is handled by main, not the read-verb path.
        assert!(
            parse(&["helm", "exec", "web", "uptime"])
                .cmd
                .unwrap()
                .as_read_verb()
                .is_none()
        );
    }

    #[test]
    fn gated_mutation_tail_includes_yes() {
        let cli = parse(&["helm", "vultr", "reboot", "abc-123", "--yes"]);
        let cmd = cli.cmd.unwrap();
        let (verb, tail) = cmd.as_read_verb().unwrap();
        assert_eq!(verb, "vultr");
        assert_eq!(tail, &["reboot", "abc-123", "--yes"]);
    }

    #[test]
    fn unknown_verb_is_an_error() {
        assert!(Cli::try_parse_from(["helm", "bogus-verb"]).is_err());
    }
}
