//! Per-business DNS sanity check.
//!
//! Local-only: shells out to `drill` (preferred, ships with libunbound on
//! OpenBSD + most BSDs) or `dig` (fallback for distros without drill) and
//! collects A / AAAA / MX / CAA records for each business's
//! `primary_domain`. The pane caller cross-references the A set against
//! the host's declared `hostname` when that hostname is an IP literal,
//! producing a per-business verdict.
//!
//! Each tool's quiet mode emits one record per line:
//!   drill -Q <domain> <type>
//!   dig +short <domain> <type>
//! The parser is total — empty stdout means NXDOMAIN or "no records",
//! never a crash.

use std::net::IpAddr;
use std::process::Command;
use std::sync::mpsc::{channel, Receiver};
use std::thread;

use crate::config::Business;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DnsCheck {
    pub business: String,
    pub domain: String,
    pub expected_ip: Option<String>,
    pub a: Vec<String>,
    pub aaaa: Vec<String>,
    pub mx: Vec<String>,
    pub caa: Vec<String>,
    pub verdict: DnsVerdict,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DnsVerdict {
    /// No expected IP supplied (host hostname is a DNS name, not a
    /// literal) — nothing to compare against.
    #[default]
    Unknown,
    /// A records include the expected IP literal.
    Match,
    /// A records do NOT include the expected IP literal.
    Mismatch,
    /// Probe error (tool missing, all queries failed).
    Error,
}

impl DnsCheck {
    /// Compute the verdict from the parsed records + expected IP. Called
    /// at the end of `probe_one` so callers see a stable value.
    pub fn compute_verdict(&mut self) {
        if self.error.is_some() {
            self.verdict = DnsVerdict::Error;
            return;
        }
        let Some(expected) = self.expected_ip.as_deref() else {
            self.verdict = DnsVerdict::Unknown;
            return;
        };
        if expected.parse::<IpAddr>().is_err() {
            // Operator's hostname is a DNS name, not an IP literal — no
            // comparison to make. Keep Unknown so the UI doesn't flag it.
            self.verdict = DnsVerdict::Unknown;
            return;
        }
        if self.a.iter().any(|ip| ip == expected) {
            self.verdict = DnsVerdict::Match;
        } else {
            self.verdict = DnsVerdict::Mismatch;
        }
    }
}

#[derive(Debug)]
pub struct DnsResult {
    pub idx: usize,
    pub check: DnsCheck,
}

/// Spawn one thread per business. `expected_ips[i]` is the IP literal we
/// expect for `businesses[i]` (or None when the host's hostname is a DNS
/// name, no IP comparison possible).
pub fn spawn_dns(
    businesses: &[Business],
    expected_ips: &[Option<String>],
) -> Receiver<DnsResult> {
    let (tx, rx) = channel();
    for (idx, biz) in businesses.iter().enumerate() {
        if biz.primary_domain.trim().is_empty() {
            continue;
        }
        let name = biz.name.clone();
        let domain = biz.primary_domain.clone();
        let expected = expected_ips.get(idx).cloned().flatten();
        let tx = tx.clone();
        thread::spawn(move || {
            let check = probe_one(&name, &domain, expected);
            let _ = tx.send(DnsResult { idx, check });
        });
    }
    rx
}

fn probe_one(name: &str, domain: &str, expected_ip: Option<String>) -> DnsCheck {
    let mut c = DnsCheck {
        business: name.to_string(),
        domain: domain.to_string(),
        expected_ip,
        ..Default::default()
    };

    let tool = pick_tool();
    let Some(tool) = tool else {
        c.error = Some("no DNS tool found (need `drill` or `dig` on PATH)".into());
        c.compute_verdict();
        return c;
    };

    let mut errs: Vec<String> = Vec::new();
    for (rtype, slot) in [
        ("A", &mut c.a),
        ("AAAA", &mut c.aaaa),
        ("MX", &mut c.mx),
        ("CAA", &mut c.caa),
    ] {
        match query(tool, domain, rtype) {
            Ok(lines) => *slot = lines,
            Err(e) => errs.push(format!("{rtype}: {e}")),
        }
    }
    if !errs.is_empty() && c.a.is_empty() && c.aaaa.is_empty() && c.mx.is_empty() && c.caa.is_empty() {
        c.error = Some(errs.join("; "));
    }
    c.compute_verdict();
    c
}

#[derive(Debug, Clone, Copy)]
enum Tool {
    Drill,
    Dig,
}

fn pick_tool() -> Option<Tool> {
    // PATH lookup via `Command::new(...).output()` with `-h` would also
    // work; simpler to just check `which`.
    for (name, t) in [("drill", Tool::Drill), ("dig", Tool::Dig)] {
        if Command::new("which")
            .arg(name)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Some(t);
        }
    }
    None
}

fn query(tool: Tool, domain: &str, rtype: &str) -> Result<Vec<String>, String> {
    let output = match tool {
        Tool::Drill => Command::new("drill")
            .args(["-Q", domain, rtype])
            .output(),
        Tool::Dig => Command::new("dig")
            .args(["+short", domain, rtype])
            .output(),
    };
    match output {
        Ok(o) if o.status.success() => {
            let body = String::from_utf8_lossy(&o.stdout);
            Ok(parse_lines(&body, rtype))
        }
        Ok(o) => Err(format!(
            "exit {}: {}",
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("spawn failed: {e}")),
    }
}

/// Split stdout into non-blank lines. For MX records the order is
/// `<pref> <host>` — preserve as-is. CAA: `<flags> <tag> "<value>"`.
pub fn parse_lines(stdout: &str, _rtype: &str) -> Vec<String> {
    stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with(';'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lines_keeps_non_blank_non_comment() {
        let out = "\n93.184.215.14\n\n; comment\n203.0.113.5\n";
        let v = parse_lines(out, "A");
        assert_eq!(v, vec!["93.184.215.14".to_string(), "203.0.113.5".into()]);
    }

    #[test]
    fn parse_lines_empty_on_no_records() {
        assert!(parse_lines("", "A").is_empty());
        assert!(parse_lines("\n\n", "A").is_empty());
    }

    #[test]
    fn verdict_match_when_a_contains_expected() {
        let mut c = DnsCheck {
            expected_ip: Some("93.184.215.14".into()),
            a: vec!["93.184.215.14".into(), "203.0.113.5".into()],
            ..Default::default()
        };
        c.compute_verdict();
        assert_eq!(c.verdict, DnsVerdict::Match);
    }

    #[test]
    fn verdict_mismatch_when_a_excludes_expected() {
        let mut c = DnsCheck {
            expected_ip: Some("93.184.215.14".into()),
            a: vec!["1.2.3.4".into()],
            ..Default::default()
        };
        c.compute_verdict();
        assert_eq!(c.verdict, DnsVerdict::Mismatch);
    }

    #[test]
    fn verdict_unknown_when_no_expected_ip() {
        let mut c = DnsCheck {
            expected_ip: None,
            a: vec!["1.2.3.4".into()],
            ..Default::default()
        };
        c.compute_verdict();
        assert_eq!(c.verdict, DnsVerdict::Unknown);
    }

    #[test]
    fn verdict_unknown_when_expected_is_dns_name_not_literal() {
        let mut c = DnsCheck {
            expected_ip: Some("router.example.org".into()),
            a: vec!["1.2.3.4".into()],
            ..Default::default()
        };
        c.compute_verdict();
        assert_eq!(c.verdict, DnsVerdict::Unknown);
    }

    #[test]
    fn verdict_error_propagates() {
        let mut c = DnsCheck {
            error: Some("oops".into()),
            ..Default::default()
        };
        c.compute_verdict();
        assert_eq!(c.verdict, DnsVerdict::Error);
    }

    #[test]
    fn ipv6_literal_matches() {
        let mut c = DnsCheck {
            expected_ip: Some("2606:2800:220:1:248:1893:25c8:1946".into()),
            a: vec!["2606:2800:220:1:248:1893:25c8:1946".into()],
            ..Default::default()
        };
        c.compute_verdict();
        assert_eq!(c.verdict, DnsVerdict::Match);
    }
}
