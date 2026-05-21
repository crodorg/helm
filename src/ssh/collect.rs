//! Fire-and-collect remote command helper.
//!
//! Unlike `ssh::run::spawn_remote` (which streams stdout/stderr lines to the
//! UI live and watches for password prompts), this captures the whole stdout
//! of a single quick command. Used by inventory pulls (`rcctl ls`, `ps`,
//! `netstat`) where the user wants results, not a stream.
//!
//! Three rcctl pulls are fired in parallel from `spawn_rcctl_triple`. The
//! caller polls the returned Receiver from the event loop and merges results
//! via `inventory::services::parse_rcctl` once all three slots arrive.

use std::process::Command;
use std::sync::mpsc::{channel, Receiver};
use std::thread;

use crate::config::OsFamily;
use crate::inventory::services::{
    parse_launchctl, parse_rcctl, parse_systemctl, Service,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    On,
    Started,
    Failed,
}

impl Slot {
    fn subcommand(self) -> &'static str {
        match self {
            Slot::On => "on",
            Slot::Started => "started",
            Slot::Failed => "failed",
        }
    }
}


/// Final, OS-normalized result a Services pane consumes — one parsed
/// `Vec<Service>` regardless of which init system produced it.
#[derive(Debug)]
pub struct ServicesResult {
    pub output: Result<Vec<Service>, String>,
}

/// OS-aware service inventory. Dispatches to:
/// - OpenBSD (`rcctl`): three parallel `rcctl ls {on,started,failed}`
///   calls wrapped in `doas -n`, merged via `parse_rcctl`.
/// - Debian (`systemctl`): single `systemctl list-units --type=service
///   --all --no-legend --plain --no-pager`. Runs unprivileged — no `sudo`
///   prefix — since listing is read-only.
/// - macOS (`launchctl`): single `launchctl list`. Runs in the user's
///   launchd domain (no system services).
///
/// Sends exactly one `ServicesResult` on the returned channel.
pub fn spawn_services(alias: &str, os: OsFamily) -> Receiver<ServicesResult> {
    let (tx, rx) = channel();
    let alias = alias.to_string();
    thread::spawn(move || {
        let result = match os {
            OsFamily::Openbsd => collect_rcctl(&alias),
            OsFamily::Linux => collect_systemctl(&alias),
            OsFamily::Macos => collect_launchctl(&alias),
        };
        let _ = tx.send(ServicesResult { output: result });
    });
    rx
}

fn collect_rcctl(alias: &str) -> Result<Vec<Service>, String> {
    // Three parallel ssh threads — `rcctl ls started|failed` each call
    // `_rc_check` per service, which is slow over the wire. Serializing
    // them tripled pane latency in practice. Each thread joins via the
    // mpsc channel; the parent collects all three before computing.
    let (tx, rx) = channel::<(Slot, Result<String, String>)>();
    for slot in [Slot::On, Slot::Started, Slot::Failed] {
        let alias = alias.to_string();
        let tx = tx.clone();
        thread::spawn(move || {
            let sub = slot.subcommand();
            let out = match Command::new("ssh")
                .arg(&alias)
                .arg(format!("doas -n rcctl ls {sub}"))
                .output()
            {
                Ok(o) if o.status.success() => {
                    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
                }
                Ok(o) => Err(format!(
                    "doas -n rcctl ls {sub} exit {}: {}",
                    o.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&o.stderr).trim()
                )),
                Err(e) => Err(format!("spawn ssh failed: {e}")),
            };
            let _ = tx.send((slot, out));
        });
    }
    drop(tx);
    let mut on = String::new();
    let mut started = String::new();
    let mut failed = String::new();
    for _ in 0..3 {
        let (slot, out) = rx.recv().map_err(|e| format!("rcctl channel: {e}"))?;
        let s = out?;
        match slot {
            Slot::On => on = s,
            Slot::Started => started = s,
            Slot::Failed => failed = s,
        }
    }
    Ok(parse_rcctl(&on, &started, &failed))
}

fn collect_systemctl(alias: &str) -> Result<Vec<Service>, String> {
    let cmd = "systemctl list-units --type=service --all --no-legend --plain --no-pager";
    run_remote(alias, cmd).map(|s| parse_systemctl(&s))
}

fn collect_launchctl(alias: &str) -> Result<Vec<Service>, String> {
    run_remote(alias, "launchctl list").map(|s| parse_launchctl(&s))
}

fn run_remote(alias: &str, cmd: &str) -> Result<String, String> {
    let exec = if alias == crate::tmux::LOCAL_ALIAS {
        Command::new("sh").arg("-c").arg(cmd).output()
    } else {
        Command::new("ssh").arg(alias).arg(cmd).output()
    };
    match exec {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => Err(format!(
            "{cmd} exit {}: {}",
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("spawn failed: {e}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvSlot {
    Processes,
    Ports,
}

#[derive(Debug)]
pub struct InvResult {
    pub slot: InvSlot,
    pub output: Result<String, String>,
}

/// Spawn two parallel pulls for the Processes pane:
/// - `ps -axo %cpu,%mem,rss,pid,user,command` (top by CPU)
/// - `netstat -na` (all sockets, parser filters to listeners)
pub fn spawn_processes_and_ports(alias: &str) -> Receiver<InvResult> {
    let (tx, rx) = channel();
    let pairs: [(InvSlot, &str); 2] = [
        (InvSlot::Processes, "ps -axo %cpu,%mem,rss,pid,user,command"),
        (InvSlot::Ports, "netstat -na"),
    ];
    for (slot, cmd) in pairs {
        let alias = alias.to_string();
        let cmd = cmd.to_string();
        let tx = tx.clone();
        thread::spawn(move || {
            let result = match Command::new("ssh").arg(&alias).arg(&cmd).output() {
                Ok(o) if o.status.success() => {
                    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
                }
                Ok(o) => Err(format!(
                    "{cmd} exit {}: {}",
                    o.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&o.stderr).trim()
                )),
                Err(e) => Err(format!("spawn ssh failed: {e}")),
            };
            let _ = tx.send(InvResult {
                slot,
                output: result,
            });
        });
    }
    rx
}

#[derive(Debug)]
pub struct TmuxListResult {
    pub alias: String,
    pub output: Result<Vec<String>, String>,
}

/// Fan out `tmux list-sessions` queries across every alias in `aliases` plus
/// the reserved `local` alias. Each thread runs `tmux::list(alias)` (which
/// shells out to ssh for remote aliases and `sh -c` for `local`) and sends a
/// single `TmuxListResult`. Returns `(total_count, receiver)` so the caller
/// can know when all results have arrived.
///
/// Used by the sessions TUI pane to enumerate `helm-*` sessions across the
/// fleet in parallel.
pub fn spawn_tmux_list_all(aliases: Vec<String>) -> (usize, Receiver<TmuxListResult>) {
    let (tx, rx) = channel();
    let mut all = aliases;
    if !all.iter().any(|a| a == crate::tmux::LOCAL_ALIAS) {
        all.push(crate::tmux::LOCAL_ALIAS.to_string());
    }
    let count = all.len();
    for alias in all {
        let tx = tx.clone();
        thread::spawn(move || {
            let output = crate::tmux::list(&alias).map_err(|e| format!("{e}"));
            let _ = tx.send(TmuxListResult { alias, output });
        });
    }
    (count, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_subcommand_strings() {
        assert_eq!(Slot::On.subcommand(), "on");
        assert_eq!(Slot::Started.subcommand(), "started");
        assert_eq!(Slot::Failed.subcommand(), "failed");
    }

    #[test]
    fn spawn_tmux_list_all_adds_local_when_missing() {
        // Use bogus aliases so ssh fails fast — we only care about the
        // total count + that `local` is included.
        let (count, rx) = spawn_tmux_list_all(vec!["bogus-alias-zzz".into()]);
        assert_eq!(count, 2, "expected user alias + auto-added local");
        let mut aliases = Vec::new();
        for _ in 0..count {
            if let Ok(r) = rx.recv() {
                aliases.push(r.alias);
            }
        }
        aliases.sort();
        assert_eq!(aliases, vec!["bogus-alias-zzz", "local"]);
    }

    #[test]
    fn spawn_tmux_list_all_keeps_explicit_local() {
        let (count, _rx) = spawn_tmux_list_all(vec!["local".into()]);
        assert_eq!(count, 1, "local already present should not be doubled");
    }
}
