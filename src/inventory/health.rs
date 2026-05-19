//! Per-business health: HTTPS reachability + TLS cert expiry.
//!
//! Both checks run *locally* from the operator's machine — no ssh. The
//! parsers in this file consume:
//!
//! - `openssl x509 -noout -enddate` → one line `notAfter=Jul 15 12:34:56 2026 GMT`
//! - `curl -s -o /dev/null -w "%{http_code} %{time_total}"` → `200 0.234`
//!
//! Each parser is total: it returns `Option<…>` and never panics on
//! garbage input, so flaky network output or unexpected openssl versions
//! degrade gracefully into "unknown" cells rather than crashing the pane.

use std::process::Command;
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Business;

/// One row of the health pane. Either field may be `None` if its check
/// failed or hasn't completed yet.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Health {
    pub business: String,
    pub domain: String,
    pub http_status: Option<u16>,
    pub http_ms: Option<u32>,
    /// Unix-seconds NotAfter from the leaf cert, if we got that far.
    pub tls_not_after_unix: Option<i64>,
    /// Reason the check failed (one of the two probes, or both).
    pub error: Option<String>,
}

impl Health {
    pub fn tls_days_left(&self, now_unix: i64) -> Option<i64> {
        self.tls_not_after_unix.map(|t| (t - now_unix) / 86_400)
    }
}

/// Best-effort wall-clock seconds. Falls back to 0 if the system clock is
/// somehow before the epoch (impossible in practice).
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parse `notAfter=MMM DD HH:MM:SS YYYY GMT` from `openssl x509 -noout -enddate`.
/// Returns unix seconds. The format is locale-independent — openssl always
/// emits English month names in this representation.
pub fn parse_openssl_enddate(s: &str) -> Option<i64> {
    let rest = s.trim().strip_prefix("notAfter=")?.trim();
    // "Jul 15 12:34:56 2026 GMT"
    let mut toks = rest.split_whitespace();
    let mon = toks.next()?;
    let day: u32 = toks.next()?.parse().ok()?;
    let hms = toks.next()?;
    let year: i32 = toks.next()?.parse().ok()?;
    // Trailing "GMT" is required to be GMT; ignored.
    let _tz = toks.next()?;
    let mut hms_iter = hms.split(':');
    let hour: u32 = hms_iter.next()?.parse().ok()?;
    let min: u32 = hms_iter.next()?.parse().ok()?;
    let sec: u32 = hms_iter.next()?.parse().ok()?;

    let month: u32 = match mon {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    civil_to_unix(year, month, day, hour, min, sec)
}

/// Parse `<status> <secs>` (e.g. `200 0.234`) into `(status, milliseconds)`.
pub fn parse_curl_w(s: &str) -> Option<(u16, u32)> {
    let mut toks = s.split_whitespace();
    let status: u16 = toks.next()?.parse().ok()?;
    let secs: f64 = toks.next()?.parse().ok()?;
    let ms = (secs * 1000.0).round();
    let ms = if ms.is_finite() && ms >= 0.0 && ms <= u32::MAX as f64 {
        ms as u32
    } else {
        return None;
    };
    Some((status, ms))
}

/// Convert proleptic-Gregorian civil date/time (UTC) → unix seconds. Based
/// on Howard Hinnant's `days_from_civil` algorithm. Saturates implausible
/// inputs by returning None.
fn civil_to_unix(y: i32, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> Option<i64> {
    if !(1..=12).contains(&m)
        || !(1..=31).contains(&d)
        || hh > 23
        || mm > 59
        || ss > 59
    {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as u32; // [0, 399]
    let m_u = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * m_u + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days_since_epoch = (era as i64) * 146097 + doe as i64 - 719468;
    Some(days_since_epoch * 86_400 + hh as i64 * 3600 + mm as i64 * 60 + ss as i64)
}

/// One completed probe result, sent over the mpsc channel by `spawn_health`.
/// `idx` is the original index into the businesses slice so the caller can
/// place the row deterministically rather than racing.
#[derive(Debug)]
pub struct HealthResult {
    pub idx: usize,
    pub health: Health,
}

/// Fire one local thread per business that has a `primary_domain`. Each
/// thread runs `openssl s_client … | openssl x509 -noout -enddate` and
/// `curl -s -o /dev/null -w …` against `https://<domain>`, then sends one
/// `HealthResult`. Businesses without a `primary_domain` are skipped (no
/// result emitted), so the caller should compute the expected total from
/// the input slice itself.
pub fn spawn_health(businesses: &[Business]) -> Receiver<HealthResult> {
    let (tx, rx) = channel();
    for (idx, biz) in businesses.iter().enumerate() {
        if biz.primary_domain.trim().is_empty() {
            continue;
        }
        let domain = biz.primary_domain.clone();
        let name = biz.name.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            let h = probe_one(&name, &domain);
            let _ = tx.send(HealthResult { idx, health: h });
        });
    }
    rx
}

fn probe_one(name: &str, domain: &str) -> Health {
    let mut h = Health {
        business: name.to_string(),
        domain: domain.to_string(),
        ..Default::default()
    };
    let mut errs: Vec<String> = Vec::new();

    // HTTP probe — curl in one shot.
    let url = format!("https://{domain}");
    match Command::new("curl")
        .args([
            "-s",
            "-o",
            "/dev/null",
            "-m",
            "10",
            "-w",
            "%{http_code} %{time_total}",
            &url,
        ])
        .output()
    {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout);
            match parse_curl_w(&s) {
                Some((status, ms)) => {
                    h.http_status = Some(status);
                    h.http_ms = Some(ms);
                }
                None => errs.push(format!("curl output unparseable: {s}")),
            }
        }
        Ok(o) => errs.push(format!(
            "curl exit {}: {}",
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => errs.push(format!("curl spawn failed: {e}")),
    }

    // TLS probe — openssl s_client → openssl x509. Run via /bin/sh so the
    // pipe is one Command. `< /dev/null` gives s_client an EOF so it exits
    // after the handshake instead of staying interactive.
    let pipeline = format!(
        "openssl s_client -servername {d} -connect {d}:443 < /dev/null 2>/dev/null \
         | openssl x509 -noout -enddate",
        d = domain
    );
    match Command::new("/bin/sh").arg("-c").arg(&pipeline).output() {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout);
            match parse_openssl_enddate(&s) {
                Some(t) => h.tls_not_after_unix = Some(t),
                None => errs.push(format!("openssl enddate unparseable: {s}")),
            }
        }
        Ok(o) => errs.push(format!(
            "openssl exit {}: {}",
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => errs.push(format!("openssl spawn failed: {e}")),
    }

    if !errs.is_empty() {
        h.error = Some(errs.join("; "));
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openssl_enddate_typical() {
        let unix =
            parse_openssl_enddate("notAfter=Jul 15 12:34:56 2026 GMT").expect("parses");
        // 2026-07-15 12:34:56 UTC
        assert_eq!(unix, 1_784_118_896);
    }

    #[test]
    fn parses_openssl_enddate_with_whitespace_padding() {
        let unix = parse_openssl_enddate(
            "  notAfter=Jan 01 00:00:00 2030 GMT  \n",
        )
        .expect("parses");
        // 2030-01-01 00:00:00 UTC
        assert_eq!(unix, 1_893_456_000);
    }

    #[test]
    fn rejects_malformed_enddate() {
        assert!(parse_openssl_enddate("notValid").is_none());
        assert!(parse_openssl_enddate("notAfter=garbage").is_none());
        assert!(parse_openssl_enddate("notAfter=Foo 01 00:00:00 2030 GMT").is_none());
        // Month out of range (parse_openssl_enddate would map name; we
        // synthesize a bad civil tuple directly).
        assert!(civil_to_unix(2030, 13, 1, 0, 0, 0).is_none());
    }

    #[test]
    fn parses_curl_w_typical() {
        assert_eq!(parse_curl_w("200 0.234"), Some((200, 234)));
        assert_eq!(parse_curl_w("503 1.5"), Some((503, 1500)));
        assert_eq!(parse_curl_w("200 0"), Some((200, 0)));
    }

    #[test]
    fn rejects_malformed_curl_w() {
        assert!(parse_curl_w("").is_none());
        assert!(parse_curl_w("200").is_none());
        assert!(parse_curl_w("xx 0.1").is_none());
        assert!(parse_curl_w("200 abc").is_none());
    }

    #[test]
    fn tls_days_left_subtracts_correctly() {
        let h = Health {
            tls_not_after_unix: Some(1_784_118_896), // 2026-07-15 12:34:56 UTC
            ..Default::default()
        };
        // 2026-05-18 00:00:00 UTC = 1_779_062_400
        // diff = 5_056_496 s = 58.5 days → 58 (integer floor toward zero)
        assert_eq!(h.tls_days_left(1_779_062_400), Some(58));
    }

    #[test]
    fn tls_days_left_negative_when_expired() {
        let h = Health {
            tls_not_after_unix: Some(1_779_062_400), // 2026-05-18 00:00:00
            ..Default::default()
        };
        // 2026-05-28 00:00:00 = 1_779_926_400, 10 days after expiry
        assert_eq!(h.tls_days_left(1_779_926_400), Some(-10));
    }
}
