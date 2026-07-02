//! `helm vultr` — Vultr provider overlay. Read-only listing here; the
//! lifecycle actions (reboot/halt/start/snapshot) are gated behind `--yes`.
//! Reuses the existing fetchers.

use std::process::ExitCode;

use serde_json::{Value, json};

use super::{fail, parse_read_args, print_json, table, usage};
use crate::vultr::{self, VultrCache};

// ── helm vultr ──────────────────────────────────────────────────────────

pub(super) fn vultr(args: &[String]) -> ExitCode {
    // Mutation subcommands — `helm vultr <action> <instance-id> --yes`.
    // Billable/destructive, so they refuse to fire without --yes (keeping
    // them off the un-gated agent surface).
    if let Some(action) = args.first().and_then(|s| parse_action(s)) {
        return vultr_action(action, &args[1..]);
    }
    let pa = match parse_read_args(args) {
        Ok(p) => p,
        Err(e) => return usage(&e),
    };
    let key = match std::env::var("VULTR_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => return fail("VULTR_API_KEY not set"),
    };
    let cache = match vultr::fetch_vultr(key) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    if pa.json {
        print_json(&vultr_json(&cache));
    } else {
        print!("{}", render_vultr(&cache));
    }
    ExitCode::SUCCESS
}

/// Map a subcommand word to a lifecycle action, or `None` if it isn't one
/// (so bare `helm vultr` still routes to the read path).
fn parse_action(s: &str) -> Option<vultr::ActionKind> {
    use vultr::ActionKind::*;
    match s {
        "reboot" => Some(Reboot),
        "halt" => Some(Halt),
        "start" => Some(Start),
        "snapshot" => Some(Snapshot),
        _ => None,
    }
}

/// `helm vultr <action> <instance-id> --yes` — fire one lifecycle action,
/// blocking on the result.
fn vultr_action(action: vultr::ActionKind, args: &[String]) -> ExitCode {
    let mut id: Option<&str> = None;
    let mut yes = false;
    for a in args {
        match a.as_str() {
            "--yes" => yes = true,
            s if s.starts_with('-') => return usage(&format!("unknown flag `{s}`")),
            s if id.is_none() => id = Some(s),
            _ => return usage("usage: helm vultr <action> <instance-id> --yes"),
        }
    }
    let Some(id) = id else {
        return usage("usage: helm vultr <action> <instance-id> --yes");
    };
    let key = match std::env::var("VULTR_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => return fail("VULTR_API_KEY not set"),
    };
    if !yes {
        eprintln!(
            "helm: refusing to {} instance {id} without --yes (billable/destructive)",
            action.label()
        );
        return ExitCode::from(2);
    }
    let rx = vultr::spawn_vultr_action(key, action, id.to_string());
    match rx.recv() {
        Ok(res) => match res.outcome {
            Ok(body) => {
                eprintln!("helm: vultr {} ok", action.label());
                if !body.trim().is_empty() {
                    print!("{body}");
                }
                ExitCode::SUCCESS
            }
            Err(e) => fail(&e),
        },
        Err(e) => fail(&format!("vultr action channel: {e}")),
    }
}

fn monthly_total(cache: &VultrCache) -> f32 {
    cache
        .instances
        .iter()
        .filter_map(|i| cache.cost_for(&i.plan))
        .sum()
}

fn vultr_json(cache: &VultrCache) -> Value {
    let instances: Vec<Value> = cache
        .instances
        .iter()
        .map(|i| {
            json!({
                "id": i.id,
                "label": i.label,
                "region": i.region,
                "plan": i.plan,
                "status": i.status,
                "power_status": i.power_status,
                "main_ip": i.main_ip,
                "monthly_cost": cache.cost_for(&i.plan),
            })
        })
        .collect();
    json!({ "instances": instances, "monthly_total": monthly_total(cache) })
}

fn render_vultr(cache: &VultrCache) -> String {
    if cache.instances.is_empty() {
        return "(no Vultr instances)\n".into();
    }
    let rows: Vec<Vec<String>> = cache
        .instances
        .iter()
        .map(|i| {
            vec![
                i.label.clone(),
                i.region.clone(),
                i.plan.clone(),
                i.status.clone(),
                i.power_status.clone(),
                i.main_ip.clone(),
                cache
                    .cost_for(&i.plan)
                    .map(|c| format!("${c:.2}"))
                    .unwrap_or("-".into()),
            ]
        })
        .collect();
    format!(
        "{}\n\nmonthly total: ${:.2}\n",
        table(
            &["LABEL", "REGION", "PLAN", "STATUS", "POWER", "IP", "COST"],
            &rows
        ),
        monthly_total(cache)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_action_maps_lifecycle_verbs() {
        use vultr::ActionKind::*;
        assert_eq!(parse_action("reboot"), Some(Reboot));
        assert_eq!(parse_action("halt"), Some(Halt));
        assert_eq!(parse_action("start"), Some(Start));
        assert_eq!(parse_action("snapshot"), Some(Snapshot));
        assert_eq!(parse_action("bogus"), None);
        assert_eq!(parse_action(""), None);
    }

    #[test]
    fn vultr_total_sums_known_plans() {
        let cache = VultrCache {
            instances: vultr::parse_instances(
                r#"{"instances":[
                    {"id":"a","region":"ewr","plan":"p1","status":"active","power_status":"running","main_ip":"1.1.1.1","label":"x"},
                    {"id":"b","region":"ewr","plan":"p2","status":"active","power_status":"running","main_ip":"1.1.1.2","label":"y"}
                ]}"#,
            )
            .unwrap(),
            plans: vultr::parse_plans(
                r#"{"plans":[{"id":"p1","monthly_cost":6.0},{"id":"p2","monthly_cost":20.0}]}"#,
            )
            .unwrap(),
        };
        assert_eq!(monthly_total(&cache), 26.0);
        let out = render_vultr(&cache);
        assert!(out.contains("$6.00"));
        assert!(out.contains("monthly total: $26.00"));
    }
}
