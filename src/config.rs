use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::ssh::sshconfig::SshHost;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub hosts: Vec<Host>,
    #[serde(default)]
    pub businesses: Vec<Business>,
    #[serde(default)]
    pub ssh_config: SshConfigSection,
    #[serde(default)]
    pub shortcuts: Vec<Shortcut>,
    #[serde(default)]
    pub logs: Vec<Log>,
    #[serde(default)]
    pub features: Features,
    /// When true (default), `helm` spawns a `helm daemon` after the TUI
    /// exits so external `helm exec` calls still work. Set to false to
    /// keep the operator's machine free of background daemons (e.g.
    /// when running `helm` on a laptop only ad-hoc).
    #[serde(default)]
    pub auto_daemon: Option<bool>,
    /// Extra flags inserted after `tmux` on every tmux invocation helm
    /// makes — both the ssh'd remote scripts and the local attach. Unset
    /// defaults to `["-u"]` (force UTF-8, so box-drawing / unicode render
    /// even when the remote locale doesn't advertise it). Set
    /// `tmux_flags = []` to disable, or list your own (e.g.
    /// `["-u", "-2"]`). Resolve via [`Config::tmux_flags`].
    #[serde(default)]
    pub tmux_flags: Option<Vec<String>>,
}

/// Optional Browse-pane toggles. Helm ships several side panes that are
/// only useful with specific external dependencies (Vultr API key, custom
/// DNS resolver setup, the printing-press money CLIs). They default to
/// off so a fresh install shows only the panes everyone needs; flip the
/// relevant flag in `config.toml` to surface them.
#[derive(Debug, Clone, Deserialize)]
pub struct Features {
    #[serde(default)]
    pub health: bool,
    #[serde(default)]
    pub vultr: bool,
    #[serde(default)]
    pub dns: bool,
    #[serde(default)]
    pub money: bool,
}

impl Default for Features {
    fn default() -> Self {
        Self {
            health: false,
            vultr: false,
            dns: false,
            money: false,
        }
    }
}

impl Features {
    /// Returns true when the binding key should be visible in the Browse
    /// keys panel + the help overlay. Bindings the operator hasn't opted
    /// into stay hidden; the dispatch handler also treats them as no-ops.
    pub fn browse_key_enabled(&self, key: &str) -> bool {
        match key {
            "H" => self.health,
            "v" => self.vultr,
            "d" => self.dns,
            "m" => self.money,
            _ => true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Log {
    /// Single key that selects this log from the picker overlay.
    pub key: char,
    /// Human label shown in the picker and the tail pane header.
    pub label: String,
    /// Absolute path on the remote host. Tailed via `ssh -tt <alias> tail -F <path>`.
    pub path: String,
    /// ssh_alias values this log applies to. Empty = applies to every host.
    /// Helm filters the picker by the currently selected host.
    #[serde(default)]
    pub hosts: Vec<String>,
}

impl Log {
    pub fn applies_to(&self, alias: &str) -> bool {
        self.hosts.is_empty() || self.hosts.iter().any(|h| h == alias)
    }
}

/// Per-OS built-in log defaults — always shown in the picker for the
/// selected host so a fresh install with no `[[logs]]` config still has
/// something useful on `l`. Tail paths are conservative — they all
/// exist out of the box on the listed OS — but the operator is expected
/// to add their own `[[logs]]` entries for app-specific files.
pub fn builtin_logs(os: OsFamily) -> Vec<Log> {
    match os {
        OsFamily::Openbsd => vec![
            log_entry('m', "messages", "/var/log/messages"),
            log_entry('d', "daemon", "/var/log/daemon"),
            log_entry('a', "authlog", "/var/log/authlog"),
        ],
        OsFamily::Linux => vec![
            log_entry('s', "syslog", "/var/log/syslog"),
            log_entry('a', "auth", "/var/log/auth.log"),
            log_entry('k', "kern", "/var/log/kern.log"),
        ],
        OsFamily::Macos => vec![
            log_entry('s', "system", "/var/log/system.log"),
            log_entry('i', "install", "/var/log/install.log"),
            log_entry('w', "wifi", "/var/log/wifi.log"),
        ],
    }
}

fn log_entry(key: char, label: &str, path: &str) -> Log {
    Log {
        key,
        label: label.into(),
        path: path.into(),
        hosts: Vec::new(),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Shortcut {
    /// Single key that fires this shortcut from the palette overlay.
    pub key: char,
    /// Human label shown in the palette and history.
    pub label: String,
    /// The actual remote command. Runs through the existing Runner, so
    /// `doas` / `sudo` prompts surface the password modal.
    pub cmd: String,
    /// ssh_alias values this shortcut applies to. Empty = applies to every
    /// host. Helm filters the palette by the currently selected host.
    #[serde(default)]
    pub hosts: Vec<String>,
}

impl Shortcut {
    pub fn applies_to(&self, alias: &str) -> bool {
        self.hosts.is_empty() || self.hosts.iter().any(|h| h == alias)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Host {
    pub name: String,
    pub ssh_alias: String,
    #[serde(default)]
    pub provider: Provider,
    /// OS family — drives which service manager (`rcctl`, `systemctl`,
    /// `launchctl`) the Services pane shells out to. Defaults to OpenBSD
    /// because helm grew up on an OpenBSD fleet; set explicitly in
    /// `config.toml` for Linux / macOS hosts.
    #[serde(default)]
    pub os: OsFamily,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub notes: String,
}

impl Host {
    pub fn display_hostname(&self) -> &str {
        self.hostname.as_deref().unwrap_or("?")
    }
    pub fn display_user(&self) -> &str {
        self.user.as_deref().unwrap_or("?")
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Local,
    Vultr,
    /// BuyVM Stallion (Frantech). Helm has no API integration here —
    /// Stallion's REST API was retired — but the badge is useful for
    /// operators who run mixed fleets and want to see provenance at a
    /// glance.
    Buyvm,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OsFamily {
    #[default]
    Openbsd,
    /// Any systemd-based Linux — Debian, Ubuntu, RHEL, Arch, Devuan with
    /// systemd, etc. The Services pane shells out to `systemctl`.
    Linux,
    Macos,
}

impl OsFamily {
    pub fn label(self) -> &'static str {
        match self {
            OsFamily::Openbsd => "openbsd",
            OsFamily::Linux => "linux",
            OsFamily::Macos => "macos",
        }
    }
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Local => "LOCAL",
            Provider::Vultr => "VULTR",
            Provider::Buyvm => "BUYVM",
            Provider::Unknown => "  ?  ",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Business {
    pub name: String,
    pub primary_domain: String,
    pub host: String,
    #[serde(default)]
    pub repo_path: String,
    #[serde(default)]
    pub deploy_cmd: String,
    #[serde(default)]
    pub notes: String,
    /// Optional Stripe Connect account id (e.g. `acct_1NxxxYYY`). When
    /// set, the Browse detail panel labels this business as Stripe-linked.
    /// Per-account balance fetch is deferred — the `m` pane still shows
    /// the fleet-wide Stripe view.
    #[serde(default)]
    pub stripe_account_id: Option<String>,
    /// Optional Mercury account id from `mercury-pp-cli accounts`. When
    /// set, the Browse detail panel pulls this account's available +
    /// current balance from the money cache and renders it inline.
    #[serde(default)]
    pub mercury_account_id: Option<String>,
    /// Optional Postmark server token (per Postmark "server", i.e. per
    /// business). When set, helm fires a stats fetch on startup and the
    /// Browse detail panel renders last-30-day Sent / Bounced / Spam
    /// counts. The token lives in `config.toml` (gitignored); for
    /// portfolios prefer leaving it unset and shipping the field as
    /// documentation only.
    #[serde(default)]
    pub postmark_server_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SshConfigSection {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub ignore: Vec<String>,
    /// Per-alias OS overrides for hosts discovered in ~/.ssh/config. Without
    /// this, synthesized hosts default to `openbsd`, which means the Services
    /// pane runs `rcctl ls` against a mac/linux box and fails. Add an entry
    /// per non-OpenBSD host to route the dispatcher correctly:
    ///
    /// ```toml
    /// [ssh_config.os]
    /// mac = "macos"
    /// linux-vps = "linux"
    /// ```
    ///
    /// Only consulted when synthesizing a host; explicit `[[hosts]]` entries
    /// keep their own `os` field.
    #[serde(default)]
    pub os: std::collections::HashMap<String, OsFamily>,
}

impl Default for SshConfigSection {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
            ignore: Vec::new(),
            os: std::collections::HashMap::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// Ordered list of candidate `config.toml` locations. First existing wins.
/// 1. `./config.toml` (cwd) — handy for `cargo run` and dev loops.
/// 2. Platform-native config dir via the `directories` crate:
///    - Linux/OpenBSD: `$XDG_CONFIG_HOME/helm/config.toml` (= `~/.config/helm/...`)
///    - macOS:         `~/Library/Application Support/helm/config.toml`
/// 3. XDG-style fallback at `$XDG_CONFIG_HOME/helm/config.toml` (= `~/.config/helm/...`).
///    Only meaningful on macOS, where #2 picks the Apple convention but
///    many developers reach for `~/.config/helm/...` first because that's
///    where the same config lives on their other boxes. Silently overlaps
///    on Linux.
pub fn candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join("config.toml"));
    }
    if let Some(dirs) = directories::ProjectDirs::from("", "", "helm") {
        out.push(dirs.config_dir().join("config.toml"));
    }
    if let Some(xdg) = xdg_config_helm_path() {
        out.push(xdg);
    }
    out
}

fn xdg_config_helm_path() -> Option<PathBuf> {
    let base = if let Some(v) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(v)
    } else {
        let home = std::env::var_os("HOME")?;
        let mut p = PathBuf::from(home);
        p.push(".config");
        p
    };
    Some(base.join("helm").join("config.toml"))
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_inner(true)
    }

    /// Like [`Config::load`] but without the `helm: config <path>` stderr
    /// line. Used by the `helm shell` CLI, which loads config only to pick
    /// up `tmux_flags` and is called repeatedly by the agent — the path
    /// banner would be noise on every `read`/`send`.
    pub fn load_silent() -> Result<Self> {
        Self::load_inner(false)
    }

    fn load_inner(verbose: bool) -> Result<Self> {
        for p in candidate_paths() {
            if p.exists() {
                if verbose {
                    eprintln!("helm: config {}", p.display());
                }
                return Self::load_from(&p);
            }
        }
        // No config.toml found — return defaults so ssh_config-only operation works.
        Ok(Config::default())
    }

    /// Resolved global tmux flags (see the `tmux_flags` field). Unset →
    /// `["-u"]`; an explicit empty list disables the default.
    pub fn tmux_flags(&self) -> Vec<String> {
        self.tmux_flags
            .clone()
            .unwrap_or_else(|| vec!["-u".to_string()])
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read config at {}", path.display()))?;
        let cfg: Config = toml::from_str(&raw).context("parse config TOML")?;
        Ok(cfg)
    }

    /// Merge hosts discovered in `~/.ssh/config` into `self.hosts`.
    ///
    /// Rules:
    /// - If `ssh_config.ignore` lists the alias, drop it.
    /// - If a TOML host already declares this `ssh_alias`, backfill its
    ///   `hostname` / `user` from ssh config when missing. TOML wins on
    ///   everything else (name, provider, notes).
    /// - Otherwise synthesize a new host with `provider = Unknown` (or
    ///   `Local` if hostname looks RFC1918 / loopback).
    pub fn merge_ssh_hosts(&mut self, ssh_hosts: Vec<SshHost>) {
        let ignore: std::collections::HashSet<&str> =
            self.ssh_config.ignore.iter().map(String::as_str).collect();

        for sh in ssh_hosts {
            if ignore.contains(sh.alias.as_str()) {
                continue;
            }
            if let Some(existing) = self.hosts.iter_mut().find(|h| h.ssh_alias == sh.alias) {
                if existing.hostname.is_none() {
                    existing.hostname = sh.hostname;
                }
                if existing.user.is_none() {
                    existing.user = sh.user;
                }
            } else {
                let provider = infer_provider(sh.hostname.as_deref());
                let os = self
                    .ssh_config
                    .os
                    .get(&sh.alias)
                    .copied()
                    .unwrap_or_default();
                self.hosts.push(Host {
                    name: sh.alias.clone(),
                    ssh_alias: sh.alias,
                    provider,
                    os,
                    hostname: sh.hostname,
                    user: sh.user,
                    notes: String::new(),
                });
            }
        }
    }
}

fn infer_provider(hostname: Option<&str>) -> Provider {
    let Some(h) = hostname else {
        return Provider::Unknown;
    };
    if let Ok(ip) = h.parse::<std::net::IpAddr>() {
        if ip.is_loopback() {
            return Provider::Local;
        }
        if let std::net::IpAddr::V4(v4) = ip {
            let o = v4.octets();
            let rfc1918 = o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168);
            if rfc1918 {
                return Provider::Local;
            }
        }
    } else if h == "localhost" || h.ends_with(".local") {
        return Provider::Local;
    }
    Provider::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_config() {
        let example = include_str!("../config.example.toml");
        let cfg: Config = toml::from_str(example).expect("example config parses");
        assert!(!cfg.hosts.is_empty());
        assert!(!cfg.businesses.is_empty());
        assert_eq!(cfg.hosts[0].provider, Provider::Local);
    }

    #[test]
    fn tmux_flags_default_when_unset() {
        let cfg = Config::default();
        assert_eq!(cfg.tmux_flags(), vec!["-u".to_string()]);
    }

    #[test]
    fn tmux_flags_explicit_empty_disables_default() {
        let cfg: Config = toml::from_str("tmux_flags = []").unwrap();
        assert!(cfg.tmux_flags().is_empty());
    }

    #[test]
    fn tmux_flags_custom_list_passes_through() {
        let cfg: Config = toml::from_str(r#"tmux_flags = ["-u", "-2"]"#).unwrap();
        assert_eq!(cfg.tmux_flags(), vec!["-u".to_string(), "-2".to_string()]);
    }

    #[test]
    fn unknown_provider_rejected() {
        let bad = r#"
            [[hosts]]
            name = "x"
            ssh_alias = "x"
            provider = "aws"
            hostname = "x"
            user = "x"
        "#;
        assert!(toml::from_str::<Config>(bad).is_err());
    }

    #[test]
    fn provider_defaults_to_unknown_and_fields_optional() {
        let minimal = r#"
            [[hosts]]
            name = "h"
            ssh_alias = "h"
        "#;
        let cfg: Config = toml::from_str(minimal).expect("minimal host parses");
        assert_eq!(cfg.hosts[0].provider, Provider::Unknown);
        assert!(cfg.hosts[0].hostname.is_none());
        assert!(cfg.hosts[0].user.is_none());
    }

    #[test]
    fn merge_backfills_missing_hostname_and_user() {
        let mut cfg: Config = toml::from_str(
            r#"
            [[hosts]]
            name = "vps1"
            ssh_alias = "vps1"
            provider = "vultr"
            notes = "app server"
            "#,
        )
        .unwrap();
        cfg.merge_ssh_hosts(vec![SshHost {
            alias: "vps1".into(),
            hostname: Some("203.0.113.10".into()),
            user: Some("admin".into()),
            port: None,
            identity_file: None,
        }]);
        assert_eq!(cfg.hosts.len(), 1);
        let h = &cfg.hosts[0];
        assert_eq!(h.provider, Provider::Vultr);
        assert_eq!(h.hostname.as_deref(), Some("203.0.113.10"));
        assert_eq!(h.user.as_deref(), Some("admin"));
        assert_eq!(h.notes, "app server");
    }

    #[test]
    fn merge_synthesizes_unknown_hosts_and_infers_local() {
        let mut cfg = Config::default();
        cfg.merge_ssh_hosts(vec![
            SshHost {
                alias: "router".into(),
                hostname: Some("192.168.1.1".into()),
                user: Some("admin".into()),
                port: None,
                identity_file: None,
            },
            SshHost {
                alias: "vps1".into(),
                hostname: Some("203.0.113.10".into()),
                user: Some("admin".into()),
                port: None,
                identity_file: None,
            },
        ]);
        assert_eq!(cfg.hosts.len(), 2);
        let r = cfg.hosts.iter().find(|h| h.name == "router").unwrap();
        assert_eq!(r.provider, Provider::Local);
        let t = cfg.hosts.iter().find(|h| h.name == "vps1").unwrap();
        assert_eq!(t.provider, Provider::Unknown);
    }

    #[test]
    fn merge_respects_ignore_list() {
        let mut cfg: Config = toml::from_str(
            r#"
            [ssh_config]
            ignore = ["router-git"]
            "#,
        )
        .unwrap();
        cfg.merge_ssh_hosts(vec![
            SshHost {
                alias: "router-git".into(),
                hostname: Some("192.168.1.1".into()),
                user: Some("git".into()),
                port: None,
                identity_file: None,
            },
            SshHost {
                alias: "router".into(),
                hostname: Some("192.168.1.1".into()),
                user: Some("admin".into()),
                port: None,
                identity_file: None,
            },
        ]);
        let aliases: Vec<&str> = cfg.hosts.iter().map(|h| h.ssh_alias.as_str()).collect();
        assert_eq!(aliases, vec!["router"]);
    }

    #[test]
    fn merge_applies_os_override_to_synthesized_hosts() {
        let mut cfg: Config = toml::from_str(
            r#"
            [ssh_config.os]
            mac = "macos"
            linux-vps = "linux"
            "#,
        )
        .unwrap();
        cfg.merge_ssh_hosts(vec![
            SshHost {
                alias: "mac".into(),
                hostname: Some("mac.lan".into()),
                user: Some("you".into()),
                port: None,
                identity_file: None,
            },
            SshHost {
                alias: "linux-vps".into(),
                hostname: Some("203.0.113.20".into()),
                user: Some("deploy".into()),
                port: None,
                identity_file: None,
            },
            SshHost {
                alias: "obsd".into(),
                hostname: Some("203.0.113.30".into()),
                user: Some("admin".into()),
                port: None,
                identity_file: None,
            },
        ]);
        let mac = cfg.hosts.iter().find(|h| h.name == "mac").unwrap();
        assert_eq!(mac.os, OsFamily::Macos);
        let lin = cfg.hosts.iter().find(|h| h.name == "linux-vps").unwrap();
        assert_eq!(lin.os, OsFamily::Linux);
        let obsd = cfg.hosts.iter().find(|h| h.name == "obsd").unwrap();
        assert_eq!(obsd.os, OsFamily::Openbsd, "unmapped alias keeps default");
    }

    #[test]
    fn os_override_does_not_clobber_explicit_toml_host() {
        let mut cfg: Config = toml::from_str(
            r#"
            [[hosts]]
            name = "mac"
            ssh_alias = "mac"
            os = "openbsd"

            [ssh_config.os]
            mac = "macos"
            "#,
        )
        .unwrap();
        cfg.merge_ssh_hosts(vec![SshHost {
            alias: "mac".into(),
            hostname: Some("mac.lan".into()),
            user: None,
            port: None,
            identity_file: None,
        }]);
        let mac = cfg.hosts.iter().find(|h| h.name == "mac").unwrap();
        assert_eq!(mac.os, OsFamily::Openbsd, "explicit [[hosts]].os wins");
    }
}
