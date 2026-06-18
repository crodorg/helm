//! `helm history` (SQLite run log), `helm activity` (agent audit jsonl), and
//! `helm logs <host> [key]` (remote tail).

use std::process::{Command, ExitCode};

use serde_json::{Value, json};

use super::{fail, parse_read_args, print_json, resolve_host, table, usage};
use crate::activity;
use crate::config::{Config, Host, Log, builtin_logs};
use crate::history::{HistoryStore, RunRecord, RunSource};

const DEFAULT_RECORDS: u32 = 50;
const DEFAULT_LOG_TAIL: u32 = 200;

// ── helm history ────────────────────────────────────────────────────────

pub(super) fn history(args: &[String]) -> ExitCode {
    let pa = match parse_read_args(args) {
        Ok(p) => p,
        Err(e) => return usage(&e),
    };
    let store = match HistoryStore::open_default() {
        Ok(s) => s,
        Err(e) => return fail(&format!("history db: {e}")),
    };
    let n = pa.n.unwrap_or(DEFAULT_RECORDS) as usize;
    let runs = match store.recent_runs(None, n) {
        Ok(r) => r,
        Err(e) => return fail(&format!("history query: {e}")),
    };
    if pa.json {
        print_json(&history_json(&runs));
    } else {
        print!("{}", render_history(&runs));
    }
    ExitCode::SUCCESS
}

fn source_str(s: RunSource) -> &'static str {
    match s {
        RunSource::Agent => "agent",
        RunSource::Operator => "operator",
    }
}

fn history_json(runs: &[RunRecord]) -> Value {
    Value::Array(
        runs.iter()
            .map(|r| {
                json!({
                    "started_at_unix": r.started_at_unix,
                    "source": source_str(r.source),
                    "alias": r.alias,
                    "cmd": r.cmd,
                    "exit": r.exit,
                    "duration_ms": r.duration_ms,
                })
            })
            .collect(),
    )
}

fn render_history(runs: &[RunRecord]) -> String {
    if runs.is_empty() {
        return "(no history)\n".into();
    }
    let rows: Vec<Vec<String>> = runs
        .iter()
        .map(|r| {
            vec![
                fmt_unix_utc(r.started_at_unix),
                source_str(r.source).to_string(),
                r.alias.clone(),
                r.exit.map(|e| e.to_string()).unwrap_or("-".into()),
                r.cmd.clone(),
            ]
        })
        .collect();
    format!(
        "{}\n",
        table(&["TIME(UTC)", "SOURCE", "ALIAS", "EXIT", "CMD"], &rows)
    )
}

// ── helm activity ───────────────────────────────────────────────────────

pub(super) fn activity(args: &[String]) -> ExitCode {
    let pa = match parse_read_args(args) {
        Ok(p) => p,
        Err(e) => return usage(&e),
    };
    let n = pa.n.unwrap_or(DEFAULT_RECORDS) as usize;
    let records = activity::tail(n);
    if pa.json {
        match serde_json::to_value(&records) {
            Ok(v) => print_json(&v),
            Err(e) => return fail(&format!("activity json: {e}")),
        }
    } else {
        print!("{}", render_activity(&records));
    }
    ExitCode::SUCCESS
}

fn render_activity(records: &[activity::ActivityRecord]) -> String {
    if records.is_empty() {
        return "(no activity)\n".into();
    }
    let rows: Vec<Vec<String>> = records
        .iter()
        .map(|r| {
            vec![
                fmt_unix_utc(r.ts_unix as i64),
                r.kind.label().to_string(),
                r.alias.clone(),
                r.exit.map(|e| e.to_string()).unwrap_or("-".into()),
                r.cmd.clone(),
            ]
        })
        .collect();
    format!(
        "{}\n",
        table(&["TIME(UTC)", "KIND", "ALIAS", "EXIT", "CMD"], &rows)
    )
}

// ── helm logs <host> [key] ──────────────────────────────────────────────

pub(super) fn logs(args: &[String]) -> ExitCode {
    let pa = match parse_read_args(args) {
        Ok(p) => p,
        Err(e) => return usage(&e),
    };
    let Some(name) = pa.pos.first() else {
        return usage("usage: helm logs <host> [key] [-n LINES] [-f] [--json]");
    };
    let cfg = match super::merged_config() {
        Ok(c) => c,
        Err(e) => return fail(&format!("config: {e}")),
    };
    let Some(h) = resolve_host(&cfg, name) else {
        return fail(&format!("unknown host `{name}`"));
    };
    let avail = available_logs(&cfg, h);
    // No key → list what's available for this host.
    let Some(key) = pa.pos.get(1) else {
        if pa.json {
            print_json(&logs_json(&avail));
        } else {
            print!("{}", render_logs_list(&avail, &h.name));
        }
        return ExitCode::SUCCESS;
    };
    let key_ch = key.chars().next().unwrap_or(' ');
    let Some(log) = avail.iter().find(|l| l.key == key_ch) else {
        return fail(&format!(
            "no log `{key}` for {} (try `helm logs {}`)",
            h.name, h.name
        ));
    };
    let n = pa.n.unwrap_or(DEFAULT_LOG_TAIL);
    if pa.follow {
        follow_exec(&h.ssh_alias, &log.path, n)
    } else {
        snapshot(&h.ssh_alias, &log.path, n)
    }
}

/// Config logs that apply to this host's alias, plus per-OS builtins for any
/// key not already taken by config (mirrors the TUI log picker).
fn available_logs(cfg: &Config, h: &Host) -> Vec<Log> {
    let mut out: Vec<Log> = cfg
        .logs
        .iter()
        .filter(|l| l.applies_to(&h.ssh_alias))
        .cloned()
        .collect();
    for b in builtin_logs(h.os) {
        if !out.iter().any(|l| l.key == b.key) {
            out.push(b);
        }
    }
    out
}

fn logs_json(logs: &[Log]) -> Value {
    Value::Array(
        logs.iter()
            .map(|l| json!({ "key": l.key.to_string(), "label": l.label, "path": l.path }))
            .collect(),
    )
}

fn render_logs_list(logs: &[Log], host: &str) -> String {
    if logs.is_empty() {
        return format!("(no logs configured for {host})\n");
    }
    let rows: Vec<Vec<String>> = logs
        .iter()
        .map(|l| vec![l.key.to_string(), l.label.clone(), l.path.clone()])
        .collect();
    format!("{}\n", table(&["KEY", "LABEL", "PATH"], &rows))
}

/// Stream `tail -F` live by replacing this process — Ctrl-C ends it, and we
/// inherit a tty so line-buffering flushes promptly.
fn follow_exec(alias: &str, path: &str, n: u32) -> ExitCode {
    use std::os::unix::process::CommandExt;
    let remote = format!("tail -n {n} -F {}", crate::tmux::shell_quote(path));
    let err = if alias == crate::tmux::LOCAL_ALIAS {
        Command::new("sh").arg("-c").arg(&remote).exec()
    } else {
        Command::new("ssh").arg("-t").arg(alias).arg(&remote).exec()
    };
    fail(&format!("exec tail failed: {err}"))
}

/// One-shot tail snapshot.
fn snapshot(alias: &str, path: &str, n: u32) -> ExitCode {
    let remote = format!("tail -n {n} {}", crate::tmux::shell_quote(path));
    let out = if alias == crate::tmux::LOCAL_ALIAS {
        Command::new("sh").arg("-c").arg(&remote).output()
    } else {
        Command::new("ssh").arg(alias).arg(&remote).output()
    };
    match out {
        Ok(o) if o.status.success() => {
            print!("{}", String::from_utf8_lossy(&o.stdout));
            ExitCode::SUCCESS
        }
        Ok(o) => fail(&format!(
            "tail exit {}: {}",
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => fail(&format!("spawn failed: {e}")),
    }
}

/// Unix seconds → `YYYY-MM-DD HH:MM` UTC. helm pulls in no date crate; this
/// is Howard Hinnant's `civil_from_days`, the inverse of the converter in
/// `inventory::health`.
fn fmt_unix_utc(ts: i64) -> String {
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let hh = secs / 3600;
    let mm = (secs % 3600) / 60;
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m as u32, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_unix_matches_known_timestamp() {
        // 1_779_062_400 = 2026-05-18 00:00:00 UTC (per inventory::health tests)
        assert_eq!(fmt_unix_utc(1_779_062_400), "2026-05-18 00:00");
        // 1_784_118_896 = 2026-07-15 12:34:56 UTC
        assert_eq!(fmt_unix_utc(1_784_118_896), "2026-07-15 12:34");
    }

    #[test]
    fn render_history_empty_and_rows() {
        assert!(render_history(&[]).contains("no history"));
        let runs = vec![RunRecord {
            id: 1,
            source: RunSource::Agent,
            alias: "web".into(),
            cmd: "uptime".into(),
            started_at_unix: 1_779_062_400,
            exit: Some(0),
            duration_ms: Some(12),
        }];
        let out = render_history(&runs);
        assert!(out.contains("agent"));
        assert!(out.contains("web"));
        assert!(out.contains("uptime"));
        assert!(out.contains("2026-05-18"));
    }

    #[test]
    fn available_logs_merges_builtins_without_clobbering_config() {
        let cfg: Config = toml::from_str(
            r#"
            [[hosts]]
            name = "web"
            ssh_alias = "web"
            os = "openbsd"
            [[logs]]
            key = "x"
            label = "app"
            path = "/var/www/log"
            hosts = ["web"]
            "#,
        )
        .unwrap();
        let h = &cfg.hosts[0];
        let avail = available_logs(&cfg, h);
        assert!(avail.iter().any(|l| l.key == 'x' && l.label == "app"));
        // OpenBSD builtins present too.
        assert!(
            avail
                .iter()
                .any(|l| l.key == 'm' && l.path == "/var/log/messages")
        );
    }

    #[test]
    fn logs_list_renders() {
        let logs = vec![Log {
            key: 'm',
            label: "messages".into(),
            path: "/var/log/messages".into(),
            hosts: vec![],
        }];
        assert!(render_logs_list(&logs, "web").contains("/var/log/messages"));
    }
}
