//! Minimal `~/.ssh/config` parser. Extracts Host blocks with alias, HostName,
//! User, and Port. Wildcard aliases (`*`, `?`, `!`-prefixed), IP-literal
//! aliases, and `Match`/`Include` blocks are skipped — helm only cares about
//! named, addressable hosts. Multiple aliases on one `Host` line each get
//! their own entry sharing the block's fields.

use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshHost {
    pub alias: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<PathBuf>,
}

/// Expand a leading `~/` to `$HOME/`. Other tilde forms (e.g. `~user/`) are
/// left unchanged — ssh would resolve them itself, but helm only needs the
/// path to compare against `ssh-add -l` output, which prints the agent-side
/// resolved path, so this is good enough in practice.
pub fn expand_tilde(p: &str) -> PathBuf {
    expand_tilde_with_home(p, std::env::var_os("HOME").as_deref())
}

/// Core of [`expand_tilde`] with `$HOME` injected rather than read from the
/// process env, so tests can pin it without the data-racy global `set_var`
/// (concurrent `setenv`/`getenv` is UB).
fn expand_tilde_with_home(p: &str, home: Option<&std::ffi::OsStr>) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = home
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(p)
}

pub fn default_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".ssh").join("config"))
}

pub fn load_from(path: &Path) -> Result<Vec<SshHost>> {
    let raw = std::fs::read_to_string(path)?;
    Ok(parse(&raw))
}

pub fn parse(raw: &str) -> Vec<SshHost> {
    let mut out: Vec<SshHost> = Vec::new();
    let mut current: Vec<SshHost> = Vec::new();
    let mut in_match = false;

    for line in raw.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut tokens = line.split_whitespace();
        let Some(key) = tokens.next() else { continue };
        let key_lower = key.to_ascii_lowercase();

        match key_lower.as_str() {
            "host" => {
                out.append(&mut current);
                in_match = false;
                for alias in tokens {
                    if !is_real_alias(alias) {
                        continue;
                    }
                    current.push(SshHost {
                        alias: alias.to_string(),
                        hostname: None,
                        user: None,
                        port: None,
                        identity_file: None,
                    });
                }
            }
            "match" => {
                out.append(&mut current);
                in_match = true;
            }
            "include" => {
                // Recursive Include not supported in v1; ignore.
            }
            _ if in_match => {}
            "hostname" => {
                if let Some(v) = tokens.next() {
                    for h in current.iter_mut() {
                        h.hostname = Some(v.to_string());
                    }
                }
            }
            "user" => {
                if let Some(v) = tokens.next() {
                    for h in current.iter_mut() {
                        h.user = Some(v.to_string());
                    }
                }
            }
            "port" => {
                if let Some(v) = tokens.next()
                    && let Ok(p) = v.parse::<u16>()
                {
                    for h in current.iter_mut() {
                        h.port = Some(p);
                    }
                }
            }
            "identityfile" => {
                if let Some(v) = tokens.next() {
                    let expanded = expand_tilde(v);
                    for h in current.iter_mut() {
                        // ssh honors the first IdentityFile listed in a Host block.
                        if h.identity_file.is_none() {
                            h.identity_file = Some(expanded.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out.append(&mut current);
    out
}

fn is_real_alias(s: &str) -> bool {
    if s.contains('*') || s.contains('?') || s.starts_with('!') {
        return false;
    }
    if s.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_block() {
        let raw = "Host foo\n    HostName 1.2.3.4\n    User alice\n";
        let hosts = parse(raw);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].alias, "foo");
        assert_eq!(hosts[0].hostname.as_deref(), Some("1.2.3.4"));
        assert_eq!(hosts[0].user.as_deref(), Some("alice"));
        assert_eq!(hosts[0].port, None);
    }

    #[test]
    fn skips_wildcards_and_ip_aliases() {
        let raw = "\
Host vps1 203.0.113.10
    HostName 203.0.113.10
    User admin
Host *
    User everyone
";
        let hosts = parse(raw);
        let aliases: Vec<&str> = hosts.iter().map(|h| h.alias.as_str()).collect();
        assert_eq!(aliases, vec!["vps1"]);
        assert_eq!(hosts[0].user.as_deref(), Some("admin"));
    }

    #[test]
    fn case_insensitive_keys_and_port() {
        let raw = "host web\n  hostname 203.0.113.1\n  PORT 2222\n";
        let hosts = parse(raw);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].port, Some(2222));
    }

    #[test]
    fn comments_and_blank_lines_ignored() {
        let raw = "# header\n\nHost a\n    # inline\n    HostName a.example\n";
        let hosts = parse(raw);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].hostname.as_deref(), Some("a.example"));
    }

    #[test]
    fn match_block_skipped() {
        let raw = "\
Match host bastion
    User root
Host real
    HostName 9.9.9.9
    User admin
";
        let hosts = parse(raw);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].alias, "real");
        assert_eq!(hosts[0].user.as_deref(), Some("admin"));
    }

    #[test]
    fn expands_leading_tilde_against_injected_home() {
        // HOME is injected, not mutated globally — concurrent setenv/getenv with
        // other tests (e.g. the `directories` crate reading HOME) is a data race.
        // (`parse` wires IdentityFile through `expand_tilde`; that path is
        // exercised by `parses_realistic_multi_host_config`.)
        let home = std::ffi::OsStr::new("/tmp/fakehome");
        assert_eq!(
            expand_tilde_with_home("~/.ssh/id_ed25519_vps", Some(home)),
            std::path::Path::new("/tmp/fakehome/.ssh/id_ed25519_vps")
        );
        // A non-tilde path is returned verbatim; missing HOME leaves `~/` as-is.
        assert_eq!(
            expand_tilde_with_home("/abs/key", Some(home)),
            std::path::Path::new("/abs/key")
        );
        assert_eq!(
            expand_tilde_with_home("~/x", None),
            std::path::Path::new("~/x")
        );
    }

    #[test]
    fn first_identity_file_wins() {
        let raw = "Host k\n    IdentityFile /a\n    IdentityFile /b\n";
        let hosts = parse(raw);
        assert_eq!(
            hosts[0].identity_file.as_deref(),
            Some(std::path::Path::new("/a"))
        );
    }

    #[test]
    fn parses_realistic_multi_host_config() {
        // Smoke test against a realistic shape: many blocks, two key files,
        // multi-token Host line, ControlMaster directives that should be ignored.
        let raw = "\
Host workstation
    HostName 192.168.1.31
    User admin
    IdentityFile ~/.ssh/id_ed25519

Host relay
    HostName 203.0.113.10
    User relay
    IdentityFile ~/.ssh/id_ed25519_vps

Host web
    HostName 203.0.113.20
    User admin
    Port 44456

Host app 203.0.113.30
    HostName 203.0.113.30
    User admin
    IdentitiesOnly yes
";
        let hosts = parse(raw);
        let aliases: Vec<&str> = hosts.iter().map(|h| h.alias.as_str()).collect();
        assert_eq!(aliases, vec!["workstation", "relay", "web", "app"]);
        let web = hosts.iter().find(|h| h.alias == "web").unwrap();
        assert_eq!(web.port, Some(44456));
    }
}
