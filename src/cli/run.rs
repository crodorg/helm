//! `helm run <key> <host>` — fire a configured `[[shortcuts]]` command on a
//! host. Shortcuts run arbitrary operator-defined commands, so this is a
//! gated mutation: it refuses without `--yes`, keeping it off the un-gated
//! agent surface. Runs through the shared `stream_and_record` (a direct ssh
//! spawn, no daemon): output streams live and the run lands in history.db under
//! the `operator` source.

use std::process::ExitCode;

use crate::history::RunSource;

use super::{fail, merged_config, resolve_host, usage};

pub(super) fn run(args: &[String]) -> ExitCode {
    let mut pos: Vec<&str> = Vec::new();
    let mut yes = false;
    for a in args {
        match a.as_str() {
            "--yes" => yes = true,
            s if s.starts_with('-') => return usage(&format!("unknown flag `{s}`")),
            s => pos.push(s),
        }
    }
    let (key, host) = match pos.as_slice() {
        [k, h] => (*k, *h),
        _ => return usage("usage: helm run <key> <host> --yes"),
    };
    let mut chars = key.chars();
    let (Some(key_char), None) = (chars.next(), chars.next()) else {
        return usage("helm run: <key> must be a single character");
    };

    let cfg = match merged_config() {
        Ok(c) => c,
        Err(e) => return fail(&format!("config: {e}")),
    };
    let Some(h) = resolve_host(&cfg, host) else {
        return fail(&format!("unknown host `{host}`"));
    };
    let Some(sc) = cfg.shortcuts.iter().find(|s| s.key == key_char) else {
        return fail(&format!(
            "no shortcut bound to `{key}` — add one under [[shortcuts]] in config.toml"
        ));
    };
    if !sc.applies_to(&h.ssh_alias) {
        return fail(&format!(
            "shortcut `{key}` ({}) is not enabled for {}",
            sc.label, h.ssh_alias
        ));
    }
    if !yes {
        eprintln!(
            "helm: would run shortcut `{key}` ({}) on {}:",
            sc.label, h.ssh_alias
        );
        eprintln!("  {}", sc.cmd);
        eprintln!("helm: pass --yes to execute");
        return ExitCode::from(2);
    }

    let alias = h.ssh_alias.clone();
    let cmd = sc.cmd.clone();
    crate::stream_and_record("helm run", RunSource::Operator, &alias, &cmd)
}
