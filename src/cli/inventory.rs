//! `helm svc|ps|ports <host>` — per-host inventory, reusing the same
//! collectors the TUI panes used (`ssh::collect`).

use std::process::ExitCode;

use serde_json::{Value, json};

use super::{fail, parse_read_args, print_json, resolve_host, table, usage};
use crate::inventory::ports::{self, ListeningSocket};
use crate::inventory::processes::{self, Process};
use crate::inventory::services::{Service, ServiceState};
use crate::ssh::collect::{InvSlot, spawn_processes_and_ports, spawn_services};

const DEFAULT_PS_ROWS: u32 = 15;

/// Stable machine string for a service state (JSON). The table uses the
/// shorter `ServiceState::label` (UP/DOWN/FAIL/TRANS).
fn state_str(s: ServiceState) -> &'static str {
    match s {
        ServiceState::Started => "started",
        ServiceState::Stopped => "stopped",
        ServiceState::Failed => "failed",
        ServiceState::Untracked => "untracked",
    }
}

// ── helm svc <host> ─────────────────────────────────────────────────────

pub(super) fn svc(args: &[String]) -> ExitCode {
    let pa = match parse_read_args(args) {
        Ok(p) => p,
        Err(e) => return usage(&e),
    };
    let Some(name) = pa.pos.first() else {
        return usage("usage: helm svc <host> [--json]");
    };
    let cfg = match super::merged_config() {
        Ok(c) => c,
        Err(e) => return fail(&format!("config: {e}")),
    };
    let Some(h) = resolve_host(&cfg, name) else {
        return fail(&format!("unknown host `{name}`"));
    };
    let services = match spawn_services(&h.ssh_alias, h.os).recv() {
        Ok(r) => match r.output {
            Ok(v) => v,
            Err(e) => return fail(&format!("services on {}: {e}", h.name)),
        },
        Err(e) => return fail(&format!("services channel: {e}")),
    };
    if pa.json {
        print_json(&svc_json(&services));
    } else {
        print!("{}", render_svc(&services, &h.name));
    }
    ExitCode::SUCCESS
}

fn svc_json(services: &[Service]) -> Value {
    Value::Array(
        services
            .iter()
            .map(|s| json!({ "name": s.name, "state": state_str(s.state) }))
            .collect(),
    )
}

fn render_svc(services: &[Service], host: &str) -> String {
    if services.is_empty() {
        return format!("(no services reported on {host})\n");
    }
    let rows: Vec<Vec<String>> = services
        .iter()
        .map(|s| vec![s.state.label().to_string(), s.name.clone()])
        .collect();
    format!("{}\n", table(&["STATE", "NAME"], &rows))
}

// ── helm ps <host> ──────────────────────────────────────────────────────

pub(super) fn ps(args: &[String]) -> ExitCode {
    let pa = match parse_read_args(args) {
        Ok(p) => p,
        Err(e) => return usage(&e),
    };
    let Some(name) = pa.pos.first() else {
        return usage("usage: helm ps <host> [-n ROWS] [--json]");
    };
    let cfg = match super::merged_config() {
        Ok(c) => c,
        Err(e) => return fail(&format!("config: {e}")),
    };
    let Some(h) = resolve_host(&cfg, name) else {
        return fail(&format!("unknown host `{name}`"));
    };
    let raw = match collect_slot(&h.ssh_alias, InvSlot::Processes) {
        Ok(s) => s,
        Err(e) => return fail(&format!("ps on {}: {e}", h.name)),
    };
    let n = pa.n.unwrap_or(DEFAULT_PS_ROWS) as usize;
    let procs = processes::top_by_cpu(&processes::parse(&raw), n);
    if pa.json {
        print_json(&ps_json(&procs));
    } else {
        print!("{}", render_ps(&procs, &h.name));
    }
    ExitCode::SUCCESS
}

fn ps_json(procs: &[Process]) -> Value {
    Value::Array(
        procs
            .iter()
            .map(|p| {
                json!({
                    "cpu": p.cpu,
                    "mem": p.mem,
                    "rss_kb": p.rss_kb,
                    "pid": p.pid,
                    "user": p.user,
                    "command": p.command,
                })
            })
            .collect(),
    )
}

fn render_ps(procs: &[Process], host: &str) -> String {
    if procs.is_empty() {
        return format!("(no processes reported on {host})\n");
    }
    let rows: Vec<Vec<String>> = procs
        .iter()
        .map(|p| {
            vec![
                format!("{:.1}", p.cpu),
                format!("{:.1}", p.mem),
                p.rss_kb.to_string(),
                p.pid.to_string(),
                p.user.clone(),
                p.command.clone(),
            ]
        })
        .collect();
    format!(
        "{}\n",
        table(&["%CPU", "%MEM", "RSS_KB", "PID", "USER", "COMMAND"], &rows)
    )
}

// ── helm ports <host> ───────────────────────────────────────────────────

pub(super) fn ports(args: &[String]) -> ExitCode {
    let pa = match parse_read_args(args) {
        Ok(p) => p,
        Err(e) => return usage(&e),
    };
    let Some(name) = pa.pos.first() else {
        return usage("usage: helm ports <host> [--json]");
    };
    let cfg = match super::merged_config() {
        Ok(c) => c,
        Err(e) => return fail(&format!("config: {e}")),
    };
    let Some(h) = resolve_host(&cfg, name) else {
        return fail(&format!("unknown host `{name}`"));
    };
    let raw = match collect_slot(&h.ssh_alias, InvSlot::Ports) {
        Ok(s) => s,
        Err(e) => return fail(&format!("ports on {}: {e}", h.name)),
    };
    let socks = ports::parse(&raw);
    if pa.json {
        print_json(&ports_json(&socks));
    } else {
        print!("{}", render_ports(&socks, &h.name));
    }
    ExitCode::SUCCESS
}

fn ports_json(socks: &[ListeningSocket]) -> Value {
    Value::Array(
        socks
            .iter()
            .map(|s| json!({ "proto": s.proto, "local": s.local }))
            .collect(),
    )
}

fn render_ports(socks: &[ListeningSocket], host: &str) -> String {
    if socks.is_empty() {
        return format!("(no listening sockets reported on {host})\n");
    }
    let rows: Vec<Vec<String>> = socks
        .iter()
        .map(|s| vec![s.proto.clone(), s.local.clone()])
        .collect();
    format!("{}\n", table(&["PROTO", "LOCAL"], &rows))
}

/// Drain the two-slot `spawn_processes_and_ports` channel and return the raw
/// stdout for the requested slot. The other slot's command runs in parallel
/// and is discarded (cheap; the two threads overlap).
fn collect_slot(alias: &str, want: InvSlot) -> std::result::Result<String, String> {
    let rx = spawn_processes_and_ports(alias);
    let mut found: Option<Result<String, String>> = None;
    for _ in 0..2 {
        match rx.recv() {
            Ok(r) if r.slot == want => found = Some(r.output),
            Ok(_) => {}
            Err(e) => return Err(format!("channel: {e}")),
        }
    }
    found.unwrap_or_else(|| Err("no result".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svc_table_uses_ui_labels() {
        let svcs = vec![
            Service {
                name: "sshd".into(),
                state: ServiceState::Started,
            },
            Service {
                name: "httpd".into(),
                state: ServiceState::Failed,
            },
        ];
        let out = render_svc(&svcs, "web");
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("UP") && l.contains("sshd"))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("FAIL") && l.contains("httpd"))
        );
    }

    #[test]
    fn svc_json_uses_machine_state() {
        let svcs = vec![Service {
            name: "sshd".into(),
            state: ServiceState::Untracked,
        }];
        let v = svc_json(&svcs);
        assert_eq!(v[0]["state"], "untracked");
        assert_eq!(v[0]["name"], "sshd");
    }

    #[test]
    fn svc_empty_is_friendly() {
        assert!(render_svc(&[], "web").contains("no services"));
    }

    #[test]
    fn ps_table_formats_floats() {
        let procs = vec![Process {
            cpu: 12.34,
            mem: 4.5,
            rss_kb: 6789,
            pid: 1234,
            user: "root".into(),
            command: "/usr/sbin/httpd".into(),
        }];
        let out = render_ps(&procs, "web");
        assert!(out.contains("12.3"));
        assert!(out.contains("6789"));
        assert!(out.contains("/usr/sbin/httpd"));
    }

    #[test]
    fn ports_render_and_json() {
        let socks = vec![ListeningSocket {
            proto: "tcp".into(),
            local: "*.22".into(),
        }];
        assert!(render_ports(&socks, "web").contains("tcp"));
        assert_eq!(ports_json(&socks)[0]["local"], "*.22");
    }
}
