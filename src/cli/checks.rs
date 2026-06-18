//! `helm health` + `helm dns` — per-business probes run locally from the
//! operator's machine (curl/openssl, drill/dig). Reuse the inventory
//! collectors and the TUI's expected-IP join.

use std::process::ExitCode;

use serde_json::{Value, json};

use super::{fail, parse_read_args, print_json, table, usage};
use crate::config::{Business, Config};
use crate::inventory::dns::{self, DnsCheck, DnsVerdict};
use crate::inventory::health::{self, Health};

/// Count businesses that will actually emit a result (a probe is only
/// spawned for those with a non-empty `primary_domain`).
fn domain_count(businesses: &[Business]) -> usize {
    businesses
        .iter()
        .filter(|b| !b.primary_domain.trim().is_empty())
        .count()
}

// ── helm health ─────────────────────────────────────────────────────────

pub(super) fn health(args: &[String]) -> ExitCode {
    let pa = match parse_read_args(args) {
        Ok(p) => p,
        Err(e) => return usage(&e),
    };
    let cfg = match super::merged_config() {
        Ok(c) => c,
        Err(e) => return fail(&format!("config: {e}")),
    };
    let expected = domain_count(&cfg.businesses);
    if expected == 0 {
        eprintln!("helm: no businesses with a primary_domain in config.toml");
        return ExitCode::SUCCESS;
    }
    let rx = health::spawn_health(&cfg.businesses);
    let mut rows: Vec<Option<Health>> = vec![None; cfg.businesses.len()];
    for _ in 0..expected {
        match rx.recv() {
            Ok(r) => {
                if let Some(slot) = rows.get_mut(r.idx) {
                    *slot = Some(r.health);
                }
            }
            Err(e) => return fail(&format!("health channel: {e}")),
        }
    }
    let results: Vec<Health> = rows.into_iter().flatten().collect();
    let now = health::now_unix();
    if pa.json {
        print_json(&health_json(&results, now));
    } else {
        print!("{}", render_health(&results, now));
    }
    ExitCode::SUCCESS
}

fn health_json(results: &[Health], now: i64) -> Value {
    Value::Array(
        results
            .iter()
            .map(|h| {
                json!({
                    "business": h.business,
                    "domain": h.domain,
                    "http_status": h.http_status,
                    "http_ms": h.http_ms,
                    "tls_days_left": h.tls_days_left(now),
                    "error": h.error,
                })
            })
            .collect(),
    )
}

fn render_health(results: &[Health], now: i64) -> String {
    let rows: Vec<Vec<String>> = results
        .iter()
        .map(|h| {
            vec![
                h.business.clone(),
                h.domain.clone(),
                h.http_status.map(|s| s.to_string()).unwrap_or("-".into()),
                h.http_ms.map(|m| m.to_string()).unwrap_or("-".into()),
                h.tls_days_left(now)
                    .map(|d| d.to_string())
                    .unwrap_or("-".into()),
                h.error.clone().unwrap_or_default(),
            ]
        })
        .collect();
    format!(
        "{}\n",
        table(
            &["BUSINESS", "DOMAIN", "HTTP", "MS", "TLS_DAYS", "ERROR"],
            &rows
        )
    )
}

// ── helm dns ────────────────────────────────────────────────────────────

pub(super) fn dns(args: &[String]) -> ExitCode {
    let pa = match parse_read_args(args) {
        Ok(p) => p,
        Err(e) => return usage(&e),
    };
    let cfg = match super::merged_config() {
        Ok(c) => c,
        Err(e) => return fail(&format!("config: {e}")),
    };
    let expected = domain_count(&cfg.businesses);
    if expected == 0 {
        eprintln!("helm: no businesses with a primary_domain in config.toml");
        return ExitCode::SUCCESS;
    }
    let expected_ips = expected_ips(&cfg);
    let rx = dns::spawn_dns(&cfg.businesses, &expected_ips);
    let mut rows: Vec<Option<DnsCheck>> = vec![None; cfg.businesses.len()];
    for _ in 0..expected {
        match rx.recv() {
            Ok(r) => {
                if let Some(slot) = rows.get_mut(r.idx) {
                    *slot = Some(r.check);
                }
            }
            Err(e) => return fail(&format!("dns channel: {e}")),
        }
    }
    let results: Vec<DnsCheck> = rows.into_iter().flatten().collect();
    if pa.json {
        print_json(&dns_json(&results));
    } else {
        print!("{}", render_dns(&results));
    }
    ExitCode::SUCCESS
}

/// Per-business expected IP: join each business's `host` to its `[[hosts]]`
/// entry's `hostname` (mirrors `app::spawn_dns_state`).
fn expected_ips(cfg: &Config) -> Vec<Option<String>> {
    cfg.businesses
        .iter()
        .map(|b| {
            cfg.hosts
                .iter()
                .find(|h| h.name == b.host)
                .map(|h| h.display_hostname().to_string())
        })
        .collect()
}

fn verdict_str(v: DnsVerdict) -> &'static str {
    match v {
        DnsVerdict::Unknown => "unknown",
        DnsVerdict::Match => "match",
        DnsVerdict::Mismatch => "mismatch",
        DnsVerdict::Error => "error",
    }
}

fn dns_json(results: &[DnsCheck]) -> Value {
    Value::Array(
        results
            .iter()
            .map(|c| {
                json!({
                    "business": c.business,
                    "domain": c.domain,
                    "expected_ip": c.expected_ip,
                    "verdict": verdict_str(c.verdict),
                    "a": c.a,
                    "aaaa": c.aaaa,
                    "mx": c.mx,
                    "caa": c.caa,
                    "error": c.error,
                })
            })
            .collect(),
    )
}

fn render_dns(results: &[DnsCheck]) -> String {
    let rows: Vec<Vec<String>> = results
        .iter()
        .map(|c| {
            let a = if c.a.is_empty() {
                c.error.clone().unwrap_or_else(|| "-".into())
            } else {
                c.a.join(",")
            };
            vec![
                c.business.clone(),
                c.domain.clone(),
                verdict_str(c.verdict).to_string(),
                a,
            ]
        })
        .collect();
    format!(
        "{}\n",
        table(&["BUSINESS", "DOMAIN", "VERDICT", "A / ERROR"], &rows)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_count_skips_blank() {
        let cfg: Config = toml::from_str(
            r#"
            [[businesses]]
            name = "a"
            primary_domain = "a.example"
            host = "web"
            [[businesses]]
            name = "b"
            primary_domain = ""
            host = "web"
            "#,
        )
        .unwrap();
        assert_eq!(domain_count(&cfg.businesses), 1);
    }

    #[test]
    fn expected_ips_join_host_hostname() {
        let cfg: Config = toml::from_str(
            r#"
            [[hosts]]
            name = "web"
            ssh_alias = "web"
            hostname = "203.0.113.10"
            [[businesses]]
            name = "a"
            primary_domain = "a.example"
            host = "web"
            "#,
        )
        .unwrap();
        assert_eq!(expected_ips(&cfg), vec![Some("203.0.113.10".to_string())]);
    }

    #[test]
    fn render_health_marks_unknown_cells() {
        let h = Health {
            business: "a".into(),
            domain: "a.example".into(),
            http_status: Some(200),
            http_ms: Some(123),
            tls_not_after_unix: None,
            error: None,
        };
        let out = render_health(&[h], 0);
        assert!(out.contains("200"));
        assert!(out.contains("123"));
    }

    #[test]
    fn dns_json_carries_records_and_verdict() {
        let c = DnsCheck {
            business: "a".into(),
            domain: "a.example".into(),
            expected_ip: Some("1.2.3.4".into()),
            a: vec!["1.2.3.4".into()],
            verdict: DnsVerdict::Match,
            ..Default::default()
        };
        let v = dns_json(&[c]);
        assert_eq!(v[0]["verdict"], "match");
        assert_eq!(v[0]["a"][0], "1.2.3.4");
    }
}
