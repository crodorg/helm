//! mosh transport selection for the interactive attach path.
//!
//! Only `helm shell open` (the interactive attach) can use mosh — it's a
//! roaming PTY, not a capture channel, so `send`/`read`/`exec`/inventory
//! stay on plain ssh. Selection honors the per-host `mosh` config
//! (`auto`/`on`/`off`); `auto` probes the remote once for `mosh-server` and
//! caches the verdict on disk (7-day TTL) so the common path pays no
//! per-attach round-trip.
//!
//! The decision is made *before* the caller `exec()`s, because exec replaces
//! the process and can't fall back afterward.

use std::collections::HashMap;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::{Config, MoshPref};

const CACHE_TTL_SECS: i64 = 7 * 24 * 3600;

/// Chosen transport for an interactive attach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Mosh,
    Ssh,
}

/// Decide the transport for attaching to `alias`. Resolves the per-host
/// `mosh` preference from config, checks the local client, then (for `auto`)
/// probes/caches the remote.
pub fn decide(alias: &str) -> Transport {
    decide_with(alias, pref_for(alias), local_mosh_present)
}

/// Testable core: `pref` + an injected local-client check. `auto` still does
/// the (un-injected) remote probe, so unit tests cover the no-I/O branches.
fn decide_with(alias: &str, pref: MoshPref, local_present: fn() -> bool) -> Transport {
    match pref {
        MoshPref::Off => Transport::Ssh,
        _ if !local_present() => Transport::Ssh,
        MoshPref::On => Transport::Mosh,
        MoshPref::Auto => {
            if remote_mosh_present(alias) {
                Transport::Mosh
            } else {
                Transport::Ssh
            }
        }
    }
}

/// Per-host `mosh` preference; defaults to `Auto` for aliases without an
/// explicit `[[hosts]]` entry. Reads only `[[hosts]]` (no ssh_config merge)
/// so an attach pays just a small TOML parse.
fn pref_for(alias: &str) -> MoshPref {
    Config::load_silent()
        .ok()
        .and_then(|c| {
            c.hosts
                .iter()
                .find(|h| h.ssh_alias == alias || h.name == alias)
                .map(|h| h.mosh)
        })
        .unwrap_or_default()
}

fn local_mosh_present() -> bool {
    // Binary on PATH ⇒ spawn succeeds; ErrorKind::NotFound ⇒ absent.
    Command::new("mosh").arg("--version").output().is_ok()
}

/// Cached remote `mosh-server` presence with a 7-day TTL. Inconclusive
/// probes (ssh connection failures) are not cached and fall back to ssh.
fn remote_mosh_present(alias: &str) -> bool {
    let now = now_unix();
    let mut cache = load_cache();
    if let Some(e) = cache.entries.get(alias)
        && is_fresh(e.checked_unix, now)
    {
        return e.present;
    }
    let (code, stdout_empty) = run_probe(alias);
    match interpret_probe(code, stdout_empty) {
        Some(present) => {
            cache.entries.insert(
                alias.to_string(),
                Entry {
                    present,
                    checked_unix: now,
                },
            );
            save_cache(&cache);
            present
        }
        // Inconclusive (couldn't reach the host) — don't poison the cache.
        None => false,
    }
}

/// Run `ssh <alias> command -v mosh-server`, returning (exit code, whether
/// stdout was empty). Split out so `interpret_probe` stays pure.
fn run_probe(alias: &str) -> (i32, bool) {
    match Command::new("ssh")
        .args([
            "-o",
            "ConnectTimeout=5",
            "-o",
            "BatchMode=yes",
            alias,
            "command -v mosh-server",
        ])
        .output()
    {
        Ok(o) => (
            o.status.code().unwrap_or(-1),
            o.stdout.iter().all(u8::is_ascii_whitespace),
        ),
        // Local ssh spawn failure — treat as inconclusive.
        Err(_) => (255, true),
    }
}

/// Interpret a probe result. ssh's own connection failure is exit 255 →
/// inconclusive (`None`); otherwise present iff the remote `command -v`
/// succeeded (exit 0) with non-empty output.
fn interpret_probe(code: i32, stdout_empty: bool) -> Option<bool> {
    if code == 255 {
        None
    } else {
        Some(code == 0 && !stdout_empty)
    }
}

fn is_fresh(checked_unix: i64, now: i64) -> bool {
    now >= checked_unix && now - checked_unix < CACHE_TTL_SECS
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    entries: HashMap<String, Entry>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Entry {
    present: bool,
    checked_unix: i64,
}

fn cache_path() -> Option<std::path::PathBuf> {
    crate::activity::state_dir().map(|d| d.join("mosh_cache.json"))
}

fn load_cache() -> Cache {
    let Some(p) = cache_path() else {
        return Cache::default();
    };
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_cache(cache: &Cache) {
    let Some(p) = cache_path() else { return };
    if let Ok(s) = serde_json::to_string(cache) {
        let _ = std::fs::write(&p, s);
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_is_always_ssh() {
        assert_eq!(decide_with("h", MoshPref::Off, || true), Transport::Ssh);
        assert_eq!(decide_with("h", MoshPref::Off, || false), Transport::Ssh);
    }

    #[test]
    fn no_local_client_falls_back_to_ssh() {
        // Even `on` can't use mosh without a local client.
        assert_eq!(decide_with("h", MoshPref::On, || false), Transport::Ssh);
        assert_eq!(decide_with("h", MoshPref::Auto, || false), Transport::Ssh);
    }

    #[test]
    fn on_with_local_client_uses_mosh() {
        assert_eq!(decide_with("h", MoshPref::On, || true), Transport::Mosh);
    }

    #[test]
    fn interpret_probe_distinguishes_inconclusive() {
        assert_eq!(interpret_probe(0, false), Some(true)); // found
        assert_eq!(interpret_probe(0, true), Some(false)); // exit 0 but empty
        assert_eq!(interpret_probe(1, true), Some(false)); // command not found
        assert_eq!(interpret_probe(255, true), None); // ssh connect failure
    }

    #[test]
    fn freshness_window() {
        assert!(is_fresh(1000, 1000));
        assert!(is_fresh(1000, 1000 + CACHE_TTL_SECS - 1));
        assert!(!is_fresh(1000, 1000 + CACHE_TTL_SECS));
        assert!(!is_fresh(1000, 999)); // clock went backwards → stale
    }

    #[test]
    fn cache_roundtrips() {
        let mut c = Cache::default();
        c.entries.insert(
            "web".into(),
            Entry {
                present: true,
                checked_unix: 42,
            },
        );
        let s = serde_json::to_string(&c).unwrap();
        let back: Cache = serde_json::from_str(&s).unwrap();
        assert!(back.entries["web"].present);
        assert_eq!(back.entries["web"].checked_unix, 42);
    }
}
