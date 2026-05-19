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
}
