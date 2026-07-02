//! Fire-and-collect remote inventory helpers.
//!
//! Thin wrappers over [`crate::ssh::one_shot`] (capture a quick command's whole
//! stdout) that parse the result into the inventory types. `fetch_services`
//! dispatches by OS; the OpenBSD path fans the three `rcctl ls
//! {on,started,failed}` pulls out across parallel threads — each `rcctl ls
//! started|failed` calls `_rc_check` per service, slow over the wire, so
//! serializing them tripled latency in practice — then merges them via
//! `parse_rcctl`. Every call is blocking: the caller gets the parsed result, not
//! a channel.

use std::thread;

use crate::config::OsFamily;
use crate::inventory::services::{Service, parse_launchctl, parse_rcctl, parse_systemctl};

/// OS-aware service inventory. OpenBSD → three parallel `rcctl ls
/// {on,started,failed}` (wrapped in `doas -n`), merged via `parse_rcctl`;
/// Debian → one `systemctl list-units …` (unprivileged — listing is read-only);
/// macOS → one `launchctl list` (the user's launchd domain, no system services).
pub fn fetch_services(alias: &str, os: OsFamily) -> Result<Vec<Service>, String> {
    match os {
        OsFamily::Openbsd => collect_rcctl(alias),
        OsFamily::Linux => collect_systemctl(alias),
        OsFamily::Macos => collect_launchctl(alias),
    }
}

fn collect_rcctl(alias: &str) -> Result<Vec<Service>, String> {
    // Three parallel ssh calls — `rcctl ls started|failed` each call `_rc_check`
    // per service, slow over the wire; serializing them tripled latency.
    let spawn = |sub: &'static str| {
        let alias = alias.to_string();
        thread::spawn(move || crate::ssh::one_shot(&alias, &format!("doas -n rcctl ls {sub}")))
    };
    let on = spawn("on");
    let started = spawn("started");
    let failed = spawn("failed");
    let on = join_rcctl(on, "on")?;
    let started = join_rcctl(started, "started")?;
    let failed = join_rcctl(failed, "failed")?;
    Ok(parse_rcctl(&on, &started, &failed))
}

/// Join one rcctl thread, flattening a thread panic into the error channel.
fn join_rcctl(h: thread::JoinHandle<Result<String, String>>, what: &str) -> Result<String, String> {
    h.join()
        .map_err(|_| format!("rcctl {what} thread panicked"))?
}

fn collect_systemctl(alias: &str) -> Result<Vec<Service>, String> {
    let cmd = "systemctl list-units --type=service --all --no-legend --plain --no-pager";
    crate::ssh::one_shot(alias, cmd).map(|s| parse_systemctl(&s))
}

fn collect_launchctl(alias: &str) -> Result<Vec<Service>, String> {
    crate::ssh::one_shot(alias, "launchctl list").map(|s| parse_launchctl(&s))
}

/// `ps -axo …` (top by CPU) — one round-trip; `processes::parse` sorts and
/// truncates. One ssh call, unlike the old collector that also fetched ports.
pub fn fetch_processes(alias: &str) -> Result<String, String> {
    crate::ssh::one_shot(alias, "ps -axo %cpu,%mem,rss,pid,user,command")
}

/// `netstat -na` (all sockets; `ports::parse` filters to listeners).
pub fn fetch_ports(alias: &str) -> Result<String, String> {
    crate::ssh::one_shot(alias, "netstat -na")
}
