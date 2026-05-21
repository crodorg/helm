//! OpenBSD `rcctl` service inventory.
//!
//! Three commands:
//!   rcctl ls on        → services enabled at boot
//!   rcctl ls started   → services currently running
//!   rcctl ls failed    → services that failed to start
//!
//! Each prints one service name per line. We diff the three sets to produce
//! per-service state.

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Started,
    Stopped,
    Failed,
    /// Started but not enabled at boot.
    Untracked,
}

impl ServiceState {
    pub fn label(self) -> &'static str {
        match self {
            ServiceState::Started => "UP",
            ServiceState::Stopped => "DOWN",
            ServiceState::Failed => "FAIL",
            ServiceState::Untracked => "TRANS",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Service {
    pub name: String,
    pub state: ServiceState,
}

pub fn parse_rcctl(on: &str, started: &str, failed: &str) -> Vec<Service> {
    let on = lines(on);
    let started = lines(started);
    let failed = lines(failed);

    let mut map: BTreeMap<String, ServiceState> = BTreeMap::new();

    for name in &on {
        map.insert(name.clone(), ServiceState::Stopped);
    }
    for name in &started {
        let entry = map
            .entry(name.clone())
            .or_insert(ServiceState::Untracked);
        if *entry == ServiceState::Stopped {
            *entry = ServiceState::Started;
        }
    }
    for name in &failed {
        map.insert(name.clone(), ServiceState::Failed);
    }

    map.into_iter()
        .map(|(name, state)| Service { name, state })
        .collect()
}

fn lines(s: &str) -> Vec<String> {
    s.lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Parse `systemctl list-units --type=service --all --no-legend --plain
/// --no-pager`. Each row is `UNIT LOAD ACTIVE SUB DESCRIPTION` separated
/// by runs of whitespace; the description (last field) may itself contain
/// spaces. We only care about the first four columns.
///
/// State mapping:
///   active   running          → Started
///   active   exited           → Started (one-shot completed)
///   active   *                → Started
///   inactive *                → Stopped
///   failed   *                → Failed
///   *        *                → Untracked
pub fn parse_systemctl(stdout: &str) -> Vec<Service> {
    let mut out: Vec<Service> = Vec::new();
    for raw in stdout.lines() {
        let row = raw.trim();
        if row.is_empty() {
            continue;
        }
        let mut cols = row.split_whitespace();
        let Some(unit) = cols.next() else { continue };
        if !unit.ends_with(".service") {
            continue;
        }
        let _load = cols.next();
        let active = cols.next().unwrap_or("");
        let _sub = cols.next().unwrap_or("");
        let state = match active {
            "active" => ServiceState::Started,
            "inactive" => ServiceState::Stopped,
            "failed" => ServiceState::Failed,
            _ => ServiceState::Untracked,
        };
        // Strip the `.service` suffix for a cleaner display — matches the
        // bare names rcctl emits.
        let name = unit.trim_end_matches(".service").to_string();
        out.push(Service { name, state });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Parse `launchctl list` output. Format:
///   PID    Status    Label
///   12345  0         com.apple.something
///   -      0         com.user.background
/// `PID = -` means stopped. `Status != 0` means failed last run. Header
/// line is skipped.
pub fn parse_launchctl(stdout: &str) -> Vec<Service> {
    let mut out: Vec<Service> = Vec::new();
    for (i, raw) in stdout.lines().enumerate() {
        if i == 0 && raw.trim_start().starts_with("PID") {
            continue;
        }
        let mut cols = raw.split_whitespace();
        let Some(pid) = cols.next() else { continue };
        let Some(status) = cols.next() else { continue };
        let Some(label) = cols.next() else { continue };
        let state = if status != "0" {
            // Non-zero last-exit status — surface as Failed even when
            // currently stopped; matches the user expectation that the
            // service is misbehaving.
            ServiceState::Failed
        } else if pid == "-" {
            ServiceState::Stopped
        } else {
            ServiceState::Started
        };
        out.push(Service {
            name: label.to_string(),
            state,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typical_openbsd_state() {
        let on = "sshd\nhttpd\nntpd\nsmtpd\n";
        let started = "sshd\nntpd\nsmtpd\npflogd\n";
        let failed = "httpd\n";

        let svc = parse_rcctl(on, started, failed);
        let by_name: std::collections::HashMap<_, _> =
            svc.iter().map(|s| (s.name.as_str(), s.state)).collect();

        assert_eq!(by_name["sshd"], ServiceState::Started);
        assert_eq!(by_name["ntpd"], ServiceState::Started);
        assert_eq!(by_name["httpd"], ServiceState::Failed);
        assert_eq!(by_name["pflogd"], ServiceState::Untracked);
    }

    #[test]
    fn empty_inputs() {
        assert!(parse_rcctl("", "", "").is_empty());
    }

    #[test]
    fn systemctl_typical() {
        let out = "\
ssh.service           loaded active   running OpenBSD Secure Shell server
nginx.service         loaded active   running A high performance web server
postfix.service       loaded failed   failed  Postfix Mail Transport Agent
unbound.service       loaded inactive dead    Unbound recursive DNS resolver
docker.service        loaded active   exited  Docker Application Container Engine
";
        let svc = parse_systemctl(out);
        let by_name: std::collections::HashMap<_, _> =
            svc.iter().map(|s| (s.name.as_str(), s.state)).collect();
        assert_eq!(by_name["ssh"], ServiceState::Started);
        assert_eq!(by_name["nginx"], ServiceState::Started);
        assert_eq!(by_name["postfix"], ServiceState::Failed);
        assert_eq!(by_name["unbound"], ServiceState::Stopped);
        assert_eq!(by_name["docker"], ServiceState::Started);
    }

    #[test]
    fn systemctl_skips_non_services() {
        let out = "\
foo.scope           loaded active   running A scope unit
sys-kernel.mount    loaded active   running Kernel mount
ssh.service         loaded active   running SSH
";
        let svc = parse_systemctl(out);
        assert_eq!(svc.len(), 1);
        assert_eq!(svc[0].name, "ssh");
    }

    #[test]
    fn launchctl_typical() {
        let out = "\
PID     Status  Label
12345   0       com.apple.appkit.xpc
-       0       com.user.idle-service
67890   1       com.user.flaky
-       2       com.user.dead-and-broken
";
        let svc = parse_launchctl(out);
        let by_name: std::collections::HashMap<_, _> =
            svc.iter().map(|s| (s.name.as_str(), s.state)).collect();
        assert_eq!(by_name["com.apple.appkit.xpc"], ServiceState::Started);
        assert_eq!(by_name["com.user.idle-service"], ServiceState::Stopped);
        assert_eq!(by_name["com.user.flaky"], ServiceState::Failed);
        assert_eq!(by_name["com.user.dead-and-broken"], ServiceState::Failed);
    }
}
