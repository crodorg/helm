//! ssh-agent introspection.
//!
//! Helm shells out to `ssh` for connections; that means key auth happens via
//! whatever the agent has loaded. This module checks which IdentityFile paths
//! the user's hosts depend on and whether the agent already holds them, so the
//! UI can warn at startup instead of blocking inside the runner.
//!
//! Strategy:
//! 1. Run `ssh-add -l` → set of loaded fingerprints. Also detect the
//!    distinct "no agent running" failure.
//! 2. For each unique IdentityFile referenced by an SshHost, compute its
//!    expected fingerprint via `ssh-keygen -lf <file>.pub`. Skip the file
//!    if no .pub sibling exists (helm refuses to touch the private key,
//!    which would require the passphrase).
//! 3. Diff expected vs loaded → `MissingKey { path, used_by: aliases }`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::ssh::sshconfig::SshHost;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingKey {
    pub identity_file: PathBuf,
    pub used_by: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    /// All IdentityFiles referenced by hosts are loaded (or unverifiable —
    /// missing .pub files are treated as loaded to avoid false alarms).
    Ok,
    /// Agent reachable but missing one or more keys helm hosts depend on.
    MissingKeys(Vec<MissingKey>),
    /// `ssh-add` could not contact the agent (e.g. `SSH_AUTH_SOCK` unset).
    AgentUnreachable,
    /// `ssh-add` not on PATH. No check possible.
    SshAddMissing,
}

pub fn check(ssh_hosts: &[SshHost]) -> AgentStatus {
    let loaded = match loaded_fingerprints() {
        Ok(set) => set,
        Err(AgentError::Unreachable) => return AgentStatus::AgentUnreachable,
        Err(AgentError::NotInstalled) => return AgentStatus::SshAddMissing,
    };

    // Map IdentityFile → list of host aliases using it.
    let mut by_key: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for h in ssh_hosts {
        if let Some(p) = &h.identity_file {
            by_key.entry(p.clone()).or_default().push(h.alias.clone());
        }
    }

    let mut missing = Vec::new();
    for (path, aliases) in by_key {
        let pub_path = pub_path_for(&path);
        let Some(fp) = pub_path.as_deref().and_then(fingerprint_of) else {
            // No .pub sibling, or ssh-keygen couldn't read it. Assume loaded
            // rather than risk a false alarm — user will find out at first
            // connection attempt if it's actually missing.
            continue;
        };
        if !loaded.iter().any(|l| l == &fp) {
            missing.push(MissingKey {
                identity_file: path,
                used_by: aliases,
            });
        }
    }

    if missing.is_empty() {
        AgentStatus::Ok
    } else {
        AgentStatus::MissingKeys(missing)
    }
}

#[derive(Debug)]
enum AgentError {
    Unreachable,
    NotInstalled,
}

fn loaded_fingerprints() -> Result<Vec<String>, AgentError> {
    let output = Command::new("ssh-add")
        .arg("-l")
        .output()
        .map_err(|_| AgentError::NotInstalled)?;

    // ssh-add exit codes:
    //   0 = at least one identity loaded
    //   1 = no identities (agent is up, just empty) — stdout: "The agent has no identities."
    //   2 = cannot connect to agent
    let code = output.status.code().unwrap_or(-1);
    if code == 2 {
        return Err(AgentError::Unreachable);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_ssh_add_l(&stdout))
}

fn parse_ssh_add_l(stdout: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        // Format: "<bits> <fingerprint> <comment...> (<type>)"
        // e.g. "256 SHA256:abc.../xyz ~/.ssh/id_ed25519 (ED25519)"
        let mut toks = line.split_whitespace();
        let _bits = toks.next();
        let Some(fp) = toks.next() else { continue };
        if fp.starts_with("SHA256:") || fp.starts_with("MD5:") {
            out.push(fp.to_string());
        }
    }
    out
}

fn pub_path_for(priv_path: &Path) -> Option<PathBuf> {
    let mut s = priv_path.as_os_str().to_owned();
    s.push(".pub");
    let p = PathBuf::from(s);
    if p.exists() { Some(p) } else { None }
}

fn fingerprint_of(pub_path: &Path) -> Option<String> {
    let output = Command::new("ssh-keygen")
        .arg("-lf")
        .arg(pub_path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_ssh_keygen_lf(&String::from_utf8_lossy(&output.stdout))
}

fn parse_ssh_keygen_lf(stdout: &str) -> Option<String> {
    // Format mirrors ssh-add -l: "<bits> <fingerprint> <comment> (<type>)"
    let line = stdout.lines().next()?;
    let mut toks = line.split_whitespace();
    let _bits = toks.next()?;
    let fp = toks.next()?;
    if fp.starts_with("SHA256:") || fp.starts_with("MD5:") {
        Some(fp.to_string())
    } else {
        None
    }
}

/// Render a multi-line blocking message for the terminal. Returned when the
/// agent state is not Ok — caller should print this and exit before the TUI
/// touches the terminal. Lists copy-pasteable `ssh-add` commands.
pub fn render_blocker(status: &AgentStatus, ssh_hosts: &[SshHost]) -> Option<String> {
    match status {
        AgentStatus::Ok => None,
        AgentStatus::SshAddMissing => Some(
            "helm: `ssh-add` not on PATH. Install OpenSSH client tools, then re-run helm.".into(),
        ),
        AgentStatus::AgentUnreachable => {
            let keys = unique_identity_files(ssh_hosts);
            let mut s = String::from("helm: ssh-agent is not running.\n\n");
            s.push_str("  eval $(ssh-agent)\n");
            if keys.is_empty() {
                s.push_str("  ssh-add\n");
            } else {
                for k in &keys {
                    s.push_str(&format!("  ssh-add {}\n", tildify(k)));
                }
            }
            s.push_str("\nThen re-run helm.");
            Some(s)
        }
        AgentStatus::MissingKeys(ks) => {
            let mut s = if ks.len() == 1 {
                String::from("helm: ssh-agent is missing 1 key required by your hosts.\n\n")
            } else {
                format!(
                    "helm: ssh-agent is missing {} keys required by your hosts.\n\n",
                    ks.len()
                )
            };
            let pad = ks
                .iter()
                .map(|m| tildify(&m.identity_file).len())
                .max()
                .unwrap_or(0);
            for m in ks {
                let path = tildify(&m.identity_file);
                s.push_str(&format!(
                    "  ssh-add {:<width$}    # used by {}\n",
                    path,
                    m.used_by.join(", "),
                    width = pad,
                ));
            }
            s.push_str("\nLoad it (you'll be prompted for the passphrase), then re-run helm.");
            Some(s)
        }
    }
}

fn unique_identity_files(ssh_hosts: &[SshHost]) -> Vec<PathBuf> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for h in ssh_hosts {
        if let Some(p) = &h.identity_file
            && seen.insert(p.clone())
        {
            out.push(p.clone());
        }
    }
    out
}

fn tildify(p: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if let Ok(rest) = p.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    p.display().to_string()
}

/// Render a one-line warning suitable for the footer status bar.
#[allow(dead_code)]
pub fn status_message(s: &AgentStatus) -> Option<String> {
    match s {
        AgentStatus::Ok => None,
        AgentStatus::AgentUnreachable => {
            Some("ssh-agent unreachable: eval $(ssh-agent) && ssh-add".into())
        }
        AgentStatus::SshAddMissing => Some("ssh-add not on PATH".into()),
        AgentStatus::MissingKeys(ks) => {
            let mut parts: Vec<String> = ks
                .iter()
                .map(|m| {
                    format!(
                        "{} ({})",
                        m.identity_file
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| m.identity_file.display().to_string()),
                        m.used_by.join(",")
                    )
                })
                .collect();
            parts.sort();
            Some(format!(
                "ssh-agent missing: {} — run ssh-add",
                parts.join("; ")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_add_l_two_keys() {
        let out = "\
256 SHA256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1 /home/user/.ssh/id_ed25519 (ED25519)
256 SHA256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb2 /home/user/.ssh/id_ed25519_vps (ED25519)
";
        let fps = parse_ssh_add_l(out);
        assert_eq!(fps.len(), 2);
        assert!(fps[0].starts_with("SHA256:"));
        assert!(fps[1].starts_with("SHA256:"));
    }

    #[test]
    fn parses_ssh_add_l_empty_agent() {
        let out = "The agent has no identities.\n";
        assert!(parse_ssh_add_l(out).is_empty());
    }

    #[test]
    fn parses_ssh_keygen_lf_output() {
        let out = "256 SHA256:abc123/xyz user@host (ED25519)\n";
        assert_eq!(
            parse_ssh_keygen_lf(out).as_deref(),
            Some("SHA256:abc123/xyz")
        );
    }

    #[test]
    fn status_message_formats_missing() {
        let s = AgentStatus::MissingKeys(vec![MissingKey {
            identity_file: PathBuf::from("/home/user/.ssh/id_ed25519_vps"),
            used_by: vec!["vps1".into(), "vps2".into(), "vps3".into()],
        }]);
        let msg = status_message(&s).unwrap();
        assert!(msg.contains("id_ed25519_vps"));
        assert!(msg.contains("vps1,vps2,vps3"));
        assert!(msg.contains("ssh-add"));
    }

    #[test]
    fn status_message_none_when_ok() {
        assert!(status_message(&AgentStatus::Ok).is_none());
    }

    #[test]
    fn render_blocker_none_when_ok() {
        assert!(render_blocker(&AgentStatus::Ok, &[]).is_none());
    }

    #[test]
    fn render_blocker_missing_keys_lists_ssh_add() {
        unsafe {
            std::env::set_var("HOME", "/home/user");
        }
        let status = AgentStatus::MissingKeys(vec![MissingKey {
            identity_file: PathBuf::from("/home/user/.ssh/id_ed25519_vps"),
            used_by: vec!["vps1".into(), "vps2".into(), "vps3".into()],
        }]);
        let msg = render_blocker(&status, &[]).unwrap();
        assert!(msg.contains("ssh-add ~/.ssh/id_ed25519_vps"));
        assert!(msg.contains("used by vps1, vps2, vps3"));
        assert!(msg.contains("re-run helm"));
    }

    #[test]
    fn render_blocker_unreachable_enumerates_identity_files() {
        unsafe {
            std::env::set_var("HOME", "/home/user");
        }
        let hosts = vec![
            SshHost {
                alias: "vps1".into(),
                hostname: Some("1.2.3.4".into()),
                user: Some("admin".into()),
                port: None,
                identity_file: Some(PathBuf::from("/home/user/.ssh/id_ed25519_vps")),
            },
            SshHost {
                alias: "desktop".into(),
                hostname: Some("192.168.1.31".into()),
                user: Some("admin".into()),
                port: None,
                identity_file: Some(PathBuf::from("/home/user/.ssh/id_ed25519")),
            },
        ];
        let msg = render_blocker(&AgentStatus::AgentUnreachable, &hosts).unwrap();
        assert!(msg.contains("eval $(ssh-agent)"));
        assert!(msg.contains("ssh-add ~/.ssh/id_ed25519_vps"));
        assert!(msg.contains("ssh-add ~/.ssh/id_ed25519"));
    }
}
