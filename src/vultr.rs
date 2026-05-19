//! Vultr API overlay — instance inventory + monthly cost.
//!
//! Two-endpoint fetch over `curl`:
//!
//! - `GET /v2/instances` → fleet inventory (region, plan id, status, IP, …)
//! - `GET /v2/plans`     → plan catalog (plan id → monthly_cost)
//!
//! Both endpoints fire in parallel from `spawn_vultr_fetch`. The caller
//! polls the returned `Receiver<VultrResult>` from the main loop and
//! merges results into `VultrCache` once both slots arrive.
//!
//! No new dependencies — `curl` is shelled out (same pattern as the
//! health pane), `serde_json` is already in `Cargo.toml`.
//!
//! API key handling: the key reaches `curl` via `-H "Authorization:
//! Bearer …"`. This means it is briefly visible to `ps -ax` on the local
//! machine for ~1 second per fetch. Acceptable for a single-operator
//! workstation; the alternative (writing the header to a temp file) is
//! more complex without being meaningfully safer in this threat model.
//! The key is logged nowhere.

use std::process::Command;
use std::sync::mpsc::{channel, Receiver};
use std::thread;

use serde::Deserialize;

/// One Vultr compute instance as returned by `GET /v2/instances`. Only
/// the fields helm actually surfaces are deserialized; extras are dropped.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Instance {
    pub id: String,
    pub region: String,
    pub plan: String,
    pub status: String,
    pub power_status: String,
    pub main_ip: String,
    #[serde(default)]
    pub label: String,
}

/// One Vultr plan (compute SKU). `monthly_cost` is dollars per month.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Plan {
    pub id: String,
    pub monthly_cost: f32,
}

/// Top-level shape of `GET /v2/instances`.
#[derive(Debug, Deserialize)]
struct InstancesResponse {
    instances: Vec<Instance>,
}

/// Top-level shape of `GET /v2/plans`.
#[derive(Debug, Deserialize)]
struct PlansResponse {
    plans: Vec<Plan>,
}

/// In-memory cache of both responses, joined on demand by helper methods.
#[derive(Debug, Clone)]
pub struct VultrCache {
    pub instances: Vec<Instance>,
    pub plans: Vec<Plan>,
}

impl VultrCache {
    /// Monthly cost for a plan id, if known.
    pub fn cost_for(&self, plan_id: &str) -> Option<f32> {
        self.plans
            .iter()
            .find(|p| p.id == plan_id)
            .map(|p| p.monthly_cost)
    }

    /// Find the instance whose `main_ip` exactly matches `ip`. No DNS
    /// resolution — the caller must pass a real IPv4 literal.
    pub fn instance_for_ip(&self, ip: &str) -> Option<&Instance> {
        if ip.is_empty() {
            return None;
        }
        self.instances.iter().find(|i| i.main_ip == ip)
    }
}

/// Which API endpoint produced a `VultrResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VultrSlot {
    Instances,
    Plans,
}

/// One completed endpoint fetch. `output` is the raw JSON body on success
/// or a human-readable error string on failure.
#[derive(Debug)]
pub struct VultrResult {
    pub slot: VultrSlot,
    pub output: Result<String, String>,
}

/// Fire two parallel `curl` calls (instances + plans). Each thread sends
/// a single `VultrResult` to the returned channel. Caller drains via
/// `try_recv` from the main loop.
pub fn spawn_vultr_fetch(api_key: String) -> Receiver<VultrResult> {
    let (tx, rx) = channel();
    let endpoints: [(VultrSlot, &str); 2] = [
        (VultrSlot::Instances, "https://api.vultr.com/v2/instances"),
        (VultrSlot::Plans, "https://api.vultr.com/v2/plans"),
    ];
    for (slot, url) in endpoints {
        let key = api_key.clone();
        let url = url.to_string();
        let tx = tx.clone();
        thread::spawn(move || {
            let auth = format!("Authorization: Bearer {key}");
            // 30s timeout: `/v2/plans` returns the full Vultr SKU catalog
            // (hundreds of rows across regions) and routinely takes >10s
            // on first hit. `/v2/instances` is fast but shares the same
            // ceiling for simplicity.
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
                    "vultr {url} exit {}: {}",
                    o.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&o.stderr).trim()
                )),
                Err(e) => Err(format!("vultr curl spawn failed: {e}")),
            };
            let _ = tx.send(VultrResult { slot, output: result });
        });
    }
    rx
}

/// Parse the `instances` array out of `GET /v2/instances`. Returns a
/// human-readable error on malformed JSON.
pub fn parse_instances(json: &str) -> Result<Vec<Instance>, String> {
    let resp: InstancesResponse = serde_json::from_str(json)
        .map_err(|e| format!("instances json: {e}"))?;
    Ok(resp.instances)
}

/// Parse the `plans` array out of `GET /v2/plans`.
pub fn parse_plans(json: &str) -> Result<Vec<Plan>, String> {
    let resp: PlansResponse =
        serde_json::from_str(json).map_err(|e| format!("plans json: {e}"))?;
    Ok(resp.plans)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTANCES_FIXTURE: &str = r#"{
      "instances": [
        {
          "id": "abc-1",
          "region": "ewr",
          "plan": "vc2-1c-1gb",
          "status": "active",
          "power_status": "running",
          "main_ip": "203.0.113.10",
          "label": "vps1"
        },
        {
          "id": "abc-2",
          "region": "ewr",
          "plan": "vc2-2c-4gb",
          "status": "active",
          "power_status": "stopped",
          "main_ip": "203.0.113.11",
          "label": "vps2"
        },
        {
          "id": "abc-3",
          "region": "lax",
          "plan": "vc2-1c-1gb",
          "status": "suspended",
          "power_status": "stopped",
          "main_ip": "203.0.113.12",
          "label": "vps3-old"
        }
      ],
      "meta": { "total": 3, "links": {"next": "", "prev": ""} }
    }"#;

    const PLANS_FIXTURE: &str = r#"{
      "plans": [
        { "id": "vc2-1c-1gb", "monthly_cost": 6.0, "vcpu_count": 1 },
        { "id": "vc2-2c-4gb", "monthly_cost": 20.0, "vcpu_count": 2 },
        { "id": "vhf-1c-2gb", "monthly_cost": 12.0, "vcpu_count": 1 }
      ],
      "meta": { "total": 3 }
    }"#;

    #[test]
    fn parses_instances_fixture() {
        let v = parse_instances(INSTANCES_FIXTURE).expect("parses");
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].id, "abc-1");
        assert_eq!(v[0].region, "ewr");
        assert_eq!(v[0].plan, "vc2-1c-1gb");
        assert_eq!(v[0].status, "active");
        assert_eq!(v[0].power_status, "running");
        assert_eq!(v[0].main_ip, "203.0.113.10");
        assert_eq!(v[0].label, "vps1");
        assert_eq!(v[2].status, "suspended");
    }

    #[test]
    fn parses_plans_fixture() {
        let v = parse_plans(PLANS_FIXTURE).expect("parses");
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].id, "vc2-1c-1gb");
        assert_eq!(v[0].monthly_cost, 6.0);
        assert_eq!(v[2].monthly_cost, 12.0);
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse_instances("not json").is_err());
        assert!(parse_plans("{\"plans\": \"oops\"}").is_err());
    }

    #[test]
    fn instance_for_ip_exact_match() {
        let cache = VultrCache {
            instances: parse_instances(INSTANCES_FIXTURE).unwrap(),
            plans: vec![],
        };
        let inst = cache.instance_for_ip("203.0.113.11").expect("found");
        assert_eq!(inst.id, "abc-2");
    }

    #[test]
    fn instance_for_ip_misses_on_partial() {
        let cache = VultrCache {
            instances: parse_instances(INSTANCES_FIXTURE).unwrap(),
            plans: vec![],
        };
        assert!(cache.instance_for_ip("203.0.113").is_none());
        assert!(cache.instance_for_ip("").is_none());
        assert!(cache.instance_for_ip("nope").is_none());
    }

    #[test]
    fn cost_for_known_plan() {
        let cache = VultrCache {
            instances: vec![],
            plans: parse_plans(PLANS_FIXTURE).unwrap(),
        };
        assert_eq!(cache.cost_for("vc2-1c-1gb"), Some(6.0));
        assert_eq!(cache.cost_for("vc2-2c-4gb"), Some(20.0));
        assert!(cache.cost_for("nope").is_none());
    }
}
