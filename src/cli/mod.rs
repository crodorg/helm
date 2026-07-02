//! Read-only CLI commands — the headless replacement for the TUI's panes.
//!
//! Each TUI view is re-expressed as a short positional-verb command
//! (`helm <verb> [host] [flags]`). Output is a human table by default and
//! machine JSON under `--json`; stdout carries the payload, stderr the
//! diagnostics. Commands reuse the existing `spawn_*` collectors (the same
//! ones the panes used) and block on the result channel rather than polling
//! a render loop.
//!
//! Design: every command is a thin fetch-and-print wrapper around a pure
//! `render_*` function that turns already-fetched data into a string. The
//! pure half is unit-tested; the shell-out half stays small.

mod cloud;
mod inventory;
mod records;
mod run;

use std::process::ExitCode;

use anyhow::Result;
use serde_json::{Value, json};

use crate::config::{Business, Config, Host, Provider};

/// Route a verb to its command. Returns `Some(exit)` when `verb` is a known
/// CLI command, or `None` for an unknown verb (which `main` reports as an
/// error).
pub fn dispatch(verb: &str, args: &[String]) -> Option<ExitCode> {
    let exit = match verb {
        "ls" => ls(args),
        "show" => show(args),
        "svc" | "services" => inventory::svc(args),
        "ps" => inventory::ps(args),
        "ports" => inventory::ports(args),
        "vultr" => cloud::vultr(args),
        "run" => run::run(args),
        "history" => records::history(args),
        "activity" => records::activity(args),
        "logs" | "log" => records::logs(args),
        _ => return None,
    };
    Some(exit)
}

// ── shared helpers ──────────────────────────────────────────────────────

/// Parsed common flags. `pos` holds the leftover positionals (e.g. host).
pub(crate) struct ReadArgs {
    pub json: bool,
    pub follow: bool,
    pub n: Option<u32>,
    pub pos: Vec<String>,
}

/// Pull `--json`, `-f/--follow`, `-n N` out of `args`; everything else is a
/// positional. Unknown `-flags` are an error so typos don't silently become
/// a host name.
pub(crate) fn parse_read_args(args: &[String]) -> std::result::Result<ReadArgs, String> {
    let mut out = ReadArgs {
        json: false,
        follow: false,
        n: None,
        pos: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => out.json = true,
            "-f" | "--follow" => out.follow = true,
            "-n" => {
                i += 1;
                let v = args.get(i).ok_or("-n requires a number")?;
                out.n = Some(
                    v.parse::<u32>()
                        .map_err(|_| "-n requires a positive integer".to_string())?,
                );
            }
            s if s.starts_with('-') => return Err(format!("unknown flag `{s}`")),
            s => out.pos.push(s.to_string()),
        }
        i += 1;
    }
    Ok(out)
}

/// Load config (silently — these run repeatedly under the agent) and merge
/// `~/.ssh/config` hosts, mirroring helm's normal host resolution.
pub(crate) fn merged_config() -> Result<Config> {
    let mut cfg = Config::load_silent()?;
    crate::load_ssh_hosts_for(&mut cfg);
    Ok(cfg)
}

/// Resolve a `<host>` argument against either the friendly `name` or the
/// `ssh_alias`.
pub(crate) fn resolve_host<'a>(cfg: &'a Config, name: &str) -> Option<&'a Host> {
    cfg.hosts
        .iter()
        .find(|h| h.name == name || h.ssh_alias == name)
}

/// Format a fixed-width column table. Header row + body; the last column is
/// never padded so trailing whitespace stays out of `grep`/copy-paste.
pub(crate) fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let ncols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(ncols) {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let header: Vec<String> = headers.iter().map(|s| (*s).to_string()).collect();
    let mut lines = vec![fmt_row(&header, &widths)];
    for row in rows {
        lines.push(fmt_row(row, &widths));
    }
    lines.join("\n")
}

fn fmt_row(cells: &[String], widths: &[usize]) -> String {
    let last = cells.len().saturating_sub(1);
    let mut parts: Vec<String> = Vec::with_capacity(cells.len());
    for (i, cell) in cells.iter().enumerate() {
        if i == last {
            parts.push(cell.clone());
        } else {
            let w = widths.get(i).copied().unwrap_or(0);
            let pad = w.saturating_sub(cell.chars().count());
            parts.push(format!("{cell}{}", " ".repeat(pad)));
        }
    }
    parts.join("  ").trim_end().to_string()
}

/// Pretty-print a JSON value to stdout.
pub(crate) fn print_json(v: &Value) {
    match serde_json::to_string_pretty(v) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("helm: json encode failed: {e}"),
    }
}

/// Usage error → stderr + exit 2.
pub(crate) fn usage(msg: &str) -> ExitCode {
    eprintln!("helm: {msg}");
    ExitCode::from(2)
}

/// Runtime failure → stderr + exit 1.
pub(crate) fn fail(msg: &str) -> ExitCode {
    eprintln!("helm: {msg}");
    ExitCode::FAILURE
}

/// Stable lowercase provider tag for JSON + tables.
pub(crate) fn provider_str(p: Provider) -> &'static str {
    match p {
        Provider::Local => "local",
        Provider::Vultr => "vultr",
        Provider::Buyvm => "buyvm",
        Provider::Unknown => "unknown",
    }
}

fn opt(s: &Option<String>) -> String {
    s.clone().unwrap_or_else(|| "-".into())
}

fn host_json(h: &Host) -> Value {
    json!({
        "name": h.name,
        "alias": h.ssh_alias,
        "os": h.os.label(),
        "provider": provider_str(h.provider),
        "hostname": h.hostname,
        "user": h.user,
        "mosh": h.mosh.label(),
        "notes": h.notes,
    })
}

fn business_json(b: &Business) -> Value {
    json!({
        "name": b.name,
        "primary_domain": b.primary_domain,
        "host": b.host,
        "repo_path": b.repo_path,
        "deploy_cmd": b.deploy_cmd,
        "stripe_account_id": b.stripe_account_id,
        "mercury_account_id": b.mercury_account_id,
        "postmark_linked": b.postmark_server_token.is_some(),
        "notes": b.notes,
    })
}

// ── helm ls ─────────────────────────────────────────────────────────────

fn ls(args: &[String]) -> ExitCode {
    let pa = match parse_read_args(args) {
        Ok(p) => p,
        Err(e) => return usage(&e),
    };
    let cfg = match merged_config() {
        Ok(c) => c,
        Err(e) => return fail(&format!("config: {e}")),
    };
    if pa.json {
        let arr: Vec<Value> = cfg.hosts.iter().map(host_json).collect();
        print_json(&Value::Array(arr));
        return ExitCode::SUCCESS;
    }
    print!("{}", render_ls(&cfg.hosts));
    ExitCode::SUCCESS
}

fn render_ls(hosts: &[Host]) -> String {
    if hosts.is_empty() {
        return "(no hosts — add [[hosts]] to config.toml or enable ~/.ssh/config)\n".into();
    }
    let rows: Vec<Vec<String>> = hosts
        .iter()
        .map(|h| {
            vec![
                h.name.clone(),
                h.ssh_alias.clone(),
                h.os.label().to_string(),
                provider_str(h.provider).to_string(),
                opt(&h.hostname),
                opt(&h.user),
            ]
        })
        .collect();
    format!(
        "{}\n",
        table(
            &["NAME", "ALIAS", "OS", "PROVIDER", "HOSTNAME", "USER"],
            &rows
        )
    )
}

// ── helm show <host> ────────────────────────────────────────────────────

fn show(args: &[String]) -> ExitCode {
    let pa = match parse_read_args(args) {
        Ok(p) => p,
        Err(e) => return usage(&e),
    };
    let Some(name) = pa.pos.first() else {
        return usage("usage: helm show <host> [--json]");
    };
    let cfg = match merged_config() {
        Ok(c) => c,
        Err(e) => return fail(&format!("config: {e}")),
    };
    let Some(h) = resolve_host(&cfg, name) else {
        return fail(&format!("unknown host `{name}`"));
    };
    let linked: Vec<&Business> = cfg.businesses.iter().filter(|b| b.host == h.name).collect();
    if pa.json {
        let mut v = host_json(h);
        v["businesses"] = Value::Array(linked.iter().map(|b| business_json(b)).collect());
        print_json(&v);
        return ExitCode::SUCCESS;
    }
    print!("{}", render_show(h, &linked));
    ExitCode::SUCCESS
}

fn render_show(h: &Host, linked: &[&Business]) -> String {
    let mut out = String::new();
    out.push_str(&format!("name      {}\n", h.name));
    out.push_str(&format!("alias     {}\n", h.ssh_alias));
    out.push_str(&format!("os        {}\n", h.os.label()));
    out.push_str(&format!("provider  {}\n", provider_str(h.provider)));
    out.push_str(&format!("hostname  {}\n", opt(&h.hostname)));
    out.push_str(&format!("user      {}\n", opt(&h.user)));
    out.push_str(&format!("mosh      {}\n", h.mosh.label()));
    if !h.notes.is_empty() {
        out.push_str(&format!("notes     {}\n", h.notes));
    }
    for b in linked {
        out.push_str(&format!("\nbusiness  {}\n", b.name));
        out.push_str(&format!("  domain  {}\n", b.primary_domain));
        if !b.repo_path.is_empty() {
            out.push_str(&format!("  repo    {}\n", b.repo_path));
        }
        if !b.deploy_cmd.is_empty() {
            out.push_str(&format!("  deploy  {}\n", b.deploy_cmd));
        }
        if b.stripe_account_id.is_some() {
            out.push_str("  stripe  linked\n");
        }
        if b.mercury_account_id.is_some() {
            out.push_str("  mercury linked\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_flags_and_positionals() {
        let pa = parse_read_args(&args(&["web", "--json", "-n", "5"])).unwrap();
        assert!(pa.json);
        assert_eq!(pa.n, Some(5));
        assert_eq!(pa.pos, vec!["web".to_string()]);
        assert!(!pa.follow);
    }

    #[test]
    fn parse_follow_flag() {
        let pa = parse_read_args(&args(&["web", "-f"])).unwrap();
        assert!(pa.follow);
    }

    #[test]
    fn parse_rejects_bad_n_and_unknown_flag() {
        assert!(parse_read_args(&args(&["-n", "x"])).is_err());
        assert!(parse_read_args(&args(&["-n"])).is_err());
        assert!(parse_read_args(&args(&["--nope"])).is_err());
    }

    #[test]
    fn table_pads_columns_and_trims_last() {
        let rows = vec![
            vec!["a".into(), "long-value".into()],
            vec!["bbb".into(), "x".into()],
        ];
        let t = table(&["H1", "H2"], &rows);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], "H1   H2");
        assert_eq!(lines[1], "a    long-value");
        // Last column unpadded → no trailing spaces.
        assert_eq!(lines[2], "bbb  x");
    }

    #[test]
    fn render_ls_empty_is_friendly() {
        assert!(render_ls(&[]).contains("no hosts"));
    }

    #[test]
    fn render_ls_lists_hosts() {
        let cfg: Config = toml::from_str(
            r#"
            [[hosts]]
            name = "web"
            ssh_alias = "web-alias"
            os = "linux"
            provider = "vultr"
            hostname = "203.0.113.10"
            user = "deploy"
            "#,
        )
        .unwrap();
        let out = render_ls(&cfg.hosts);
        assert!(out.contains("web"));
        assert!(out.contains("web-alias"));
        assert!(out.contains("linux"));
        assert!(out.contains("vultr"));
        assert!(out.contains("203.0.113.10"));
    }

    #[test]
    fn render_show_includes_linked_business() {
        let cfg: Config = toml::from_str(
            r#"
            [[hosts]]
            name = "web"
            ssh_alias = "web"
            [[businesses]]
            name = "acme"
            primary_domain = "acme.example"
            host = "web"
            "#,
        )
        .unwrap();
        let h = &cfg.hosts[0];
        let linked: Vec<&Business> = cfg.businesses.iter().filter(|b| b.host == h.name).collect();
        let out = render_show(h, &linked);
        assert!(out.contains("name      web"));
        assert!(out.contains("business  acme"));
        assert!(out.contains("acme.example"));
    }
}
