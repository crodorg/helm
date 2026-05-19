//! Postmark per-business email stats.
//!
//! Postmark has no first-party CLI in the printing-press toolkit, so helm
//! shells out to `curl` against the REST API directly. Each business
//! supplies its own server token via `postmark_server_token` in
//! `config.toml`; the token rides in `X-Postmark-Server-Token` and never
//! lands on argv (passed via `-H @-` over stdin).
//!
//! Endpoint: `GET https://api.postmarkapp.com/stats/outbound?fromdate=…&todate=…`
//! Range defaults to the last 30 days (UTC) for sensible at-a-glance
//! numbers; "all time" would give meaningless totals over a long-lived
//! Postmark server.
//!
//! Response shape (trimmed):
//! ```json
//! {
//!   "Sent": 123,
//!   "Bounced": 4,
//!   "BounceRate": 3.2,
//!   "SpamComplaints": 1,
//!   "SpamComplaintsRate": 0.8,
//!   "Tracked": 100
//! }
//! ```

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::config::Business;

#[derive(Debug, Clone, PartialEq)]
pub struct PostmarkStats {
    pub sent: u64,
    pub bounced: u64,
    pub bounce_rate: f64,
    pub spam_complaints: u64,
    pub spam_rate: f64,
    pub tracked: u64,
    pub from_date: String,
    pub to_date: String,
}

#[derive(Debug)]
pub struct PostmarkResult {
    pub business_name: String,
    pub outcome: Result<PostmarkStats, String>,
}

/// Spawn one thread per business that supplies a Postmark token. Each
/// thread issues one HTTPS GET and sends a single `PostmarkResult`.
/// Businesses without a token are skipped (no result emitted).
pub fn spawn_postmark_fetch(businesses: &[Business]) -> Receiver<PostmarkResult> {
    let (tx, rx) = channel();
    let (from, to) = default_date_range_utc();
    for biz in businesses {
        let Some(token) = biz.postmark_server_token.as_ref() else {
            continue;
        };
        if token.trim().is_empty() {
            continue;
        }
        let name = biz.name.clone();
        let token = token.clone();
        let from = from.clone();
        let to = to.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            let outcome = fetch_one(&token, &from, &to);
            let _ = tx.send(PostmarkResult { business_name: name, outcome });
        });
    }
    rx
}

fn fetch_one(token: &str, from: &str, to: &str) -> Result<PostmarkStats, String> {
    let url = format!(
        "https://api.postmarkapp.com/stats/outbound?fromdate={from}&todate={to}"
    );
    // Token via stdin -H @- avoids argv exposure to `ps -ax`.
    let mut child = Command::new("curl")
        .args([
            "-s",
            "-m", "10",
            "-H", "Accept: application/json",
            "-H", "@-",
            &url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl spawn failed: {e}"))?;
    {
        let stdin = child.stdin.as_mut().ok_or("curl stdin unavailable")?;
        writeln!(stdin, "X-Postmark-Server-Token: {token}")
            .map_err(|e| format!("curl stdin write failed: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("curl wait failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "curl exit {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let body = String::from_utf8_lossy(&out.stdout);
    let mut stats = parse_stats(&body)?;
    stats.from_date = from.to_string();
    stats.to_date = to.to_string();
    Ok(stats)
}

pub fn parse_stats(json: &str) -> Result<PostmarkStats, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Resp {
        #[serde(default)]
        sent: u64,
        #[serde(default)]
        bounced: u64,
        #[serde(default)]
        bounce_rate: f64,
        #[serde(default)]
        spam_complaints: u64,
        #[serde(default, rename = "SpamComplaintsRate")]
        spam_complaints_rate: f64,
        #[serde(default)]
        tracked: u64,
        // Surfacing the API's own error so the operator can fix the token.
        #[serde(default, rename = "ErrorCode")]
        error_code: Option<i64>,
        #[serde(default)]
        message: Option<String>,
    }
    let r: Resp = serde_json::from_str(json).map_err(|e| format!("postmark json: {e}"))?;
    if let (Some(code), Some(msg)) = (r.error_code, r.message.as_ref()) {
        if code != 0 {
            return Err(format!("postmark error {code}: {msg}"));
        }
    }
    Ok(PostmarkStats {
        sent: r.sent,
        bounced: r.bounced,
        bounce_rate: r.bounce_rate,
        spam_complaints: r.spam_complaints,
        spam_rate: r.spam_complaints_rate,
        tracked: r.tracked,
        from_date: String::new(),
        to_date: String::new(),
    })
}

/// (from, to) as YYYY-MM-DD UTC strings — to = today, from = today - 30d.
pub fn default_date_range_utc() -> (String, String) {
    let today_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let from_unix = today_unix - 30 * 86_400;
    (format_date(from_unix), format_date(today_unix))
}

fn format_date(unix: i64) -> String {
    // Howard Hinnant civil-from-days, same algorithm as ui::history.
    let days = unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TYPICAL: &str = r#"{
      "Sent": 1234,
      "Bounced": 12,
      "BounceRate": 0.97,
      "SpamComplaints": 2,
      "SpamComplaintsRate": 0.16,
      "Tracked": 1100,
      "WithLinkTracking": 800,
      "WithOpenTracking": 1000
    }"#;

    #[test]
    fn parses_typical_response() {
        let s = parse_stats(TYPICAL).expect("parses");
        assert_eq!(s.sent, 1234);
        assert_eq!(s.bounced, 12);
        assert!((s.bounce_rate - 0.97).abs() < 1e-9);
        assert_eq!(s.spam_complaints, 2);
        assert!((s.spam_rate - 0.16).abs() < 1e-9);
        assert_eq!(s.tracked, 1100);
    }

    #[test]
    fn surface_api_error() {
        let json = r#"{"ErrorCode": 10, "Message": "Server token invalid."}"#;
        let err = parse_stats(json).unwrap_err();
        assert!(err.contains("postmark error 10"));
        assert!(err.contains("invalid"));
    }

    #[test]
    fn errorcode_zero_does_not_short_circuit() {
        let json = r#"{"ErrorCode": 0, "Message": "OK", "Sent": 5}"#;
        let s = parse_stats(json).expect("parses");
        assert_eq!(s.sent, 5);
    }

    #[test]
    fn missing_fields_default_to_zero() {
        let s = parse_stats("{}").expect("parses");
        assert_eq!(s.sent, 0);
        assert_eq!(s.bounce_rate, 0.0);
    }

    #[test]
    fn rejects_garbage_json() {
        assert!(parse_stats("not json").is_err());
        assert!(parse_stats(r#"{"Sent": "oops"}"#).is_err());
    }

    #[test]
    fn date_format_round_trip() {
        // 2026-05-19 00:00:00 UTC = 1_779_148_800 per civil-from-days
        assert_eq!(format_date(1_779_148_800), "2026-05-19");
        assert_eq!(format_date(0), "1970-01-01");
    }

    #[test]
    fn default_date_range_is_30_days_apart() {
        let (from, to) = default_date_range_utc();
        assert_eq!(from.len(), 10);
        assert_eq!(to.len(), 10);
        assert_ne!(from, to);
    }
}
