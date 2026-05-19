//! BuyVM Stallion API overlay — service inventory + monthly cost.
//!
//! Mirrors the Vultr pane shape, but BuyVM exposes a single endpoint —
//! `GET {base}/services` — that already embeds billing + package info per
//! row, so no second `plans` call is needed.
//!
//! Endpoint base is configurable via `BUYVM_API_BASE` env var; the
//! default is `https://manage.frantech.ca/api/client` (Stallion's
//! published client API root). Operators on legacy WHMCS panels can
//! override and adjust the parser if the JSON shape diverges.
//!
//! Auth: `Authorization: Bearer $BUYVM_API_KEY`. Same trade-off as the
//! Vultr pane — key is briefly visible to `ps -ax` for the ~1s lifetime
//! of the curl call. Acceptable for single-operator workstation use.
//!
//! Parser is intentionally tolerant: every field is `#[serde(default)]`
//! so a Stallion shape change degrades fields to empty strings / zeros
//! rather than blanking the pane.

use std::process::Command;
use std::sync::mpsc::{channel, Receiver};
use std::thread;

use serde::Deserialize;

pub const DEFAULT_API_BASE: &str = "https://manage.frantech.ca/api/client";

/// One BuyVM service as returned by `GET /services`. Field names mirror
/// the Stallion JSON; alternates are caught via `serde(alias)` so legacy
/// WHMCS-style responses still parse.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Service {
    #[serde(default)]
    pub id: serde_json::Value, // numeric or string depending on panel
    #[serde(default, alias = "label", alias = "hostname", alias = "domain")]
    pub label: String,
    #[serde(default, alias = "primary_ip", alias = "primary_ipv4", alias = "main_ip")]
    pub ip: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, alias = "package", alias = "product_name", alias = "plan")]
    pub package: String,
    #[serde(default, alias = "location", alias = "datacenter", alias = "region")]
    pub location: String,
    /// Stallion returns this as a string (e.g. `"3.50"`); legacy panels
    /// return a number. `parse_monthly_cost` normalizes both.
    #[serde(default, alias = "monthly_total", alias = "monthly_cost", alias = "recurring")]
    pub monthly_raw: serde_json::Value,
}

impl Service {
    pub fn monthly_cost(&self) -> Option<f32> {
        parse_monthly_cost(&self.monthly_raw)
    }

    pub fn id_str(&self) -> String {
        match &self.id {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => String::new(),
        }
    }
}

/// Top-level shape — Stallion wraps the array in `{"data": [...]}`, but
/// some endpoints/legacy responses return a bare array. The parser tries
/// both.
#[derive(Debug, Deserialize)]
struct ServicesResponse {
    #[serde(default)]
    data: Option<Vec<Service>>,
}

#[derive(Debug, Clone)]
pub struct BuyvmCache {
    pub services: Vec<Service>,
    pub api_base: String,
}

impl BuyvmCache {
    /// Find the service whose `ip` exactly matches `ip`. Mirrors Vultr's
    /// `instance_for_ip`.
    pub fn service_for_ip(&self, ip: &str) -> Option<&Service> {
        if ip.is_empty() {
            return None;
        }
        self.services.iter().find(|s| s.ip == ip)
    }
}

#[derive(Debug)]
pub struct BuyvmResult {
    pub output: Result<String, String>,
}

/// Fire one `curl` call against `{base}/services`. Sends one
/// `BuyvmResult` and drops the channel.
pub fn spawn_buyvm_fetch(api_key: String, api_base: String) -> Receiver<BuyvmResult> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let url = format!("{}/services", api_base.trim_end_matches('/'));
        let auth = format!("Authorization: Bearer {api_key}");
        let result = match Command::new("curl")
            .args(["-sS", "-m", "30", "-H"])
            .arg(&auth)
            .arg(&url)
            .output()
        {
            Ok(o) if o.status.success() => {
                Ok(String::from_utf8_lossy(&o.stdout).into_owned())
            }
            Ok(o) => Err(format!(
                "buyvm {url} exit {}: {}",
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => Err(format!("buyvm curl spawn failed: {e}")),
        };
        let _ = tx.send(BuyvmResult { output: result });
    });
    rx
}

pub fn parse_services(json: &str) -> Result<Vec<Service>, String> {
    // Try wrapped shape first.
    if let Ok(resp) = serde_json::from_str::<ServicesResponse>(json) {
        if let Some(v) = resp.data {
            return Ok(v);
        }
    }
    // Fall back to bare-array shape.
    serde_json::from_str::<Vec<Service>>(json)
        .map_err(|e| format!("services json: {e}"))
}

/// Stallion returns `"3.50"` as a string but other panels return 3.5 as
/// a number. Tolerate both; return None for anything unparseable.
pub fn parse_monthly_cost(v: &serde_json::Value) -> Option<f32> {
    match v {
        serde_json::Value::String(s) => s.trim().parse::<f32>().ok(),
        serde_json::Value::Number(n) => n.as_f64().map(|x| x as f32),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WRAPPED_FIXTURE: &str = r#"{
      "data": [
        {
          "id": 1001,
          "label": "vps1",
          "ip": "203.0.113.20",
          "status": "Active",
          "package": "KVM-1024",
          "location": "Las Vegas",
          "monthly_total": "3.50"
        },
        {
          "id": 1002,
          "label": "vps2",
          "ip": "203.0.113.21",
          "status": "Suspended",
          "package": "KVM-2048",
          "location": "Luxembourg",
          "monthly_total": "7.00"
        }
      ]
    }"#;

    const BARE_ARRAY_FIXTURE: &str = r#"[
      {
        "id": "abc-9",
        "hostname": "legacy-vps",
        "primary_ipv4": "198.51.100.5",
        "status": "active",
        "product_name": "kvm-512",
        "datacenter": "lv",
        "monthly_cost": 2.0
      }
    ]"#;

    #[test]
    fn parses_wrapped_data_shape() {
        let v = parse_services(WRAPPED_FIXTURE).expect("parses");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].label, "vps1");
        assert_eq!(v[0].ip, "203.0.113.20");
        assert_eq!(v[0].status, "Active");
        assert_eq!(v[0].package, "KVM-1024");
        assert_eq!(v[0].location, "Las Vegas");
        assert_eq!(v[0].monthly_cost(), Some(3.5));
        assert_eq!(v[0].id_str(), "1001");
        assert_eq!(v[1].monthly_cost(), Some(7.0));
    }

    #[test]
    fn parses_bare_array_with_aliases() {
        let v = parse_services(BARE_ARRAY_FIXTURE).expect("parses");
        assert_eq!(v.len(), 1);
        // hostname → label via alias
        assert_eq!(v[0].label, "legacy-vps");
        // primary_ipv4 → ip
        assert_eq!(v[0].ip, "198.51.100.5");
        // product_name → package
        assert_eq!(v[0].package, "kvm-512");
        // datacenter → location
        assert_eq!(v[0].location, "lv");
        // numeric monthly_cost (vs string)
        assert_eq!(v[0].monthly_cost(), Some(2.0));
        assert_eq!(v[0].id_str(), "abc-9");
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse_services("not json").is_err());
        // Wrapped-but-corrupt: missing closing brace inside data
        assert!(parse_services(r#"{"data": ["#).is_err());
    }

    #[test]
    fn empty_data_array_ok() {
        let v = parse_services(r#"{"data": []}"#).expect("parses");
        assert!(v.is_empty());
    }

    #[test]
    fn service_for_ip_exact_match() {
        let cache = BuyvmCache {
            services: parse_services(WRAPPED_FIXTURE).unwrap(),
            api_base: DEFAULT_API_BASE.into(),
        };
        let s = cache.service_for_ip("203.0.113.21").expect("found");
        assert_eq!(s.label, "vps2");
    }

    #[test]
    fn service_for_ip_misses_on_partial_or_empty() {
        let cache = BuyvmCache {
            services: parse_services(WRAPPED_FIXTURE).unwrap(),
            api_base: DEFAULT_API_BASE.into(),
        };
        assert!(cache.service_for_ip("").is_none());
        assert!(cache.service_for_ip("203.0.113").is_none());
    }

    #[test]
    fn monthly_cost_parses_string_and_number() {
        assert_eq!(parse_monthly_cost(&serde_json::json!("3.50")), Some(3.5));
        assert_eq!(parse_monthly_cost(&serde_json::json!(7)), Some(7.0));
        assert_eq!(parse_monthly_cost(&serde_json::json!(null)), None);
        assert_eq!(parse_monthly_cost(&serde_json::json!("bad")), None);
    }
}
