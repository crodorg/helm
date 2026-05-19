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

#[derive(Debug)]
pub struct CollectResult {
    pub slot: Slot,
    pub output: Result<String, String>,
}

/// Spawn three parallel `ssh <alias> rcctl ls {on,started,failed}` calls.
/// Each thread sends a single `CollectResult` to the returned channel.
pub fn spawn_rcctl_triple(alias: &str) -> Receiver<CollectResult> {
    let (tx, rx) = channel();
    for slot in [Slot::On, Slot::Started, Slot::Failed] {
        let alias = alias.to_string();
        let tx = tx.clone();
        thread::spawn(move || {
            let sub = slot.subcommand();
            // `rcctl ls started|failed` calls `_rc_check` per service, which
            // needs root because some service pidfiles are root-owned. We
            // run via `doas -n` (non-interactive) so an unconfigured doas
            // surfaces a clean authorization error instead of hanging on a
            // password prompt this pane has no modal for. The operator
            // configures it once in /etc/doas.conf:
            //     permit nopass <user> cmd rcctl args ls on
            //     permit nopass <user> cmd rcctl args ls started
            //     permit nopass <user> cmd rcctl args ls failed
            let result = match Command::new("ssh")
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
            let _ = tx.send(CollectResult {
                slot,
                output: result,
            });
        });
    }
    rx
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_subcommand_strings() {
        assert_eq!(Slot::On.subcommand(), "on");
        assert_eq!(Slot::Started.subcommand(), "started");
        assert_eq!(Slot::Failed.subcommand(), "failed");
    }
}
