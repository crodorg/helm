//! Vultr API overlay — instance inventory + monthly cost.
//!
//! Two-endpoint fetch over `curl`:
//!
//! - `GET /v2/instances` → fleet inventory (region, plan id, status, IP, …)
//! - `GET /v2/plans`     → plan catalog (plan id → monthly_cost)
//!
//! Both endpoints fire in parallel (`fetch_vultr`), which blocks until both
//! land and returns the joined `VultrCache`.
//!
//! No new dependencies — `curl` is shelled out, `serde_json` is already in
//! `Cargo.toml`.
//!
//! API key handling: the key reaches `curl` via `-H "Authorization:
//! Bearer …"`. This means it is briefly visible to `ps -ax` on the local
//! machine for ~1 second per fetch. Acceptable for a single-operator
//! workstation; the alternative (writing the header to a temp file) is
//! more complex without being meaningfully safer in this threat model.
//! The key is logged nowhere.

use std::process::Command;
use std::sync::mpsc::{Receiver, channel};
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
}

/// Fetch both endpoints (instances + plans) and return the joined cache. The
/// two `curl` calls run on their own threads — `/v2/plans` returns the full SKU
/// catalog and routinely takes >10s, so overlapping them roughly halves
/// wall-clock — then this blocks until both land and parses them.
pub fn fetch_vultr(api_key: String) -> Result<VultrCache, String> {
    let instances = {
        let key = api_key.clone();
        thread::spawn(move || curl_get(&key, "https://api.vultr.com/v2/instances"))
    };
    let plans = {
        let key = api_key.clone();
        thread::spawn(move || curl_get(&key, "https://api.vultr.com/v2/plans"))
    };
    let instances = join_fetch(instances, "instances")?;
    let plans = join_fetch(plans, "plans")?;
    Ok(VultrCache {
        instances: parse_instances(&instances)?,
        plans: parse_plans(&plans)?,
    })
}

/// Join one fetch thread, flattening a thread panic into the error channel.
fn join_fetch(h: thread::JoinHandle<Result<String, String>>, what: &str) -> Result<String, String> {
    h.join()
        .map_err(|_| format!("vultr {what} thread panicked"))?
}

/// `curl` one Vultr endpoint with the bearer key, returning the raw JSON body.
fn curl_get(api_key: &str, url: &str) -> Result<String, String> {
    let auth = format!("Authorization: Bearer {api_key}");
    // 30s timeout: `/v2/plans` returns the full Vultr SKU catalog (hundreds of
    // rows across regions) and routinely takes >10s on first hit; `/v2/instances`
    // is fast but shares the ceiling for simplicity.
    match Command::new("curl")
        .args(["-sS", "-m", "30", "-H"])
        .arg(&auth)
        .arg(url)
        .output()
    {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => Err(format!(
            "vultr {url} exit {}: {}",
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("vultr curl spawn failed: {e}")),
    }
}

/// Vultr lifecycle actions. Mapping to API endpoints lives in
/// `ActionKind::endpoint_path` + `ActionKind::body_for`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Reboot,
    Halt,
    Start,
    Snapshot,
}

impl ActionKind {
    pub fn label(self) -> &'static str {
        match self {
            ActionKind::Reboot => "reboot",
            ActionKind::Halt => "halt (power off)",
            ActionKind::Start => "start (power on)",
            ActionKind::Snapshot => "snapshot",
        }
    }

    /// Path appended to `https://api.vultr.com` for this action.
    pub fn endpoint_path(self, instance_id: &str) -> String {
        match self {
            ActionKind::Reboot => format!("/v2/instances/{instance_id}/reboot"),
            ActionKind::Halt => format!("/v2/instances/{instance_id}/halt"),
            ActionKind::Start => format!("/v2/instances/{instance_id}/start"),
            ActionKind::Snapshot => "/v2/snapshots".to_string(),
        }
    }

    /// JSON body Vultr expects for this action. `Snapshot` needs the
    /// instance id in the body; the per-instance actions send no body.
    /// Built via `serde_json::json!` so any character in the id is
    /// properly escaped — Vultr ids are UUID-ish today but the parser
    /// reads them as strings, so this is defense in depth.
    pub fn body_for(self, instance_id: &str) -> Option<String> {
        match self {
            ActionKind::Snapshot => {
                Some(serde_json::json!({ "instance_id": instance_id }).to_string())
            }
            _ => None,
        }
    }
}

/// One completed action call. `outcome` is the API response (often empty
/// for 2xx) or a human-readable error string.
#[derive(Debug)]
pub struct ActionResult {
    pub outcome: Result<String, String>,
}

/// Fire one action POST in a background thread. Returns a receiver the
/// caller blocks on for the single result.
pub fn spawn_vultr_action(
    api_key: String,
    action: ActionKind,
    instance_id: String,
) -> Receiver<ActionResult> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let url = format!(
            "https://api.vultr.com{}",
            action.endpoint_path(&instance_id)
        );
        let auth = format!("Authorization: Bearer {api_key}");
        let mut cmd = Command::new("curl");
        // `-f` so an HTTP 4xx/5xx (e.g. bad instance id) is a non-zero curl
        // exit rather than a 200-shaped "success" — a billable/destructive
        // action must not report `ok` on a rejected request.
        cmd.args(["-sS", "-f", "-m", "30", "-X", "POST", "-H"])
            .arg(&auth)
            .args(["-H", "Content-Type: application/json"]);
        if let Some(body) = action.body_for(&instance_id) {
            cmd.arg("-d").arg(body);
        }
        cmd.arg(&url);
        let outcome = match cmd.output() {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
            Ok(o) => Err(format!(
                "vultr action exit {}: {}",
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => Err(format!("vultr action curl spawn failed: {e}")),
        };
        let _ = tx.send(ActionResult { outcome });
    });
    rx
}

/// Parse the `instances` array out of `GET /v2/instances`. Returns a
/// human-readable error on malformed JSON.
pub fn parse_instances(json: &str) -> Result<Vec<Instance>, String> {
    let resp: InstancesResponse =
        serde_json::from_str(json).map_err(|e| format!("instances json: {e}"))?;
    Ok(resp.instances)
}

/// Parse the `plans` array out of `GET /v2/plans`.
pub fn parse_plans(json: &str) -> Result<Vec<Plan>, String> {
    let resp: PlansResponse = serde_json::from_str(json).map_err(|e| format!("plans json: {e}"))?;
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
    fn action_endpoint_paths_are_correct() {
        assert_eq!(
            ActionKind::Reboot.endpoint_path("abc-1"),
            "/v2/instances/abc-1/reboot"
        );
        assert_eq!(
            ActionKind::Halt.endpoint_path("abc-1"),
            "/v2/instances/abc-1/halt"
        );
        assert_eq!(
            ActionKind::Start.endpoint_path("abc-1"),
            "/v2/instances/abc-1/start"
        );
        assert_eq!(ActionKind::Snapshot.endpoint_path("abc-1"), "/v2/snapshots");
    }

    #[test]
    fn snapshot_carries_instance_id_in_body() {
        assert_eq!(
            ActionKind::Snapshot.body_for("abc-1").as_deref(),
            Some(r#"{"instance_id":"abc-1"}"#)
        );
        assert_eq!(ActionKind::Reboot.body_for("abc-1"), None);
        assert_eq!(ActionKind::Halt.body_for("abc-1"), None);
        assert_eq!(ActionKind::Start.body_for("abc-1"), None);
    }

    #[test]
    fn snapshot_body_escapes_special_characters() {
        // Defense in depth — Vultr ids are alphanumeric in practice but
        // the parser doesn't enforce that. A quote or backslash in the
        // id must not break the JSON.
        let body = ActionKind::Snapshot
            .body_for(r#"weird"id\here"#)
            .expect("snapshot has body");
        // Round-trip via serde_json to confirm it's valid + correct.
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid json");
        assert_eq!(v["instance_id"], r#"weird"id\here"#);
    }

    #[test]
    fn action_labels_render_human_readable() {
        assert_eq!(ActionKind::Reboot.label(), "reboot");
        assert_eq!(ActionKind::Halt.label(), "halt (power off)");
        assert_eq!(ActionKind::Start.label(), "start (power on)");
        assert_eq!(ActionKind::Snapshot.label(), "snapshot");
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
