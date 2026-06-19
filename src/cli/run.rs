//! `helm run <key> <host>` — fire a configured `[[shortcuts]]` command on a
//! host. Shortcuts run arbitrary operator-defined commands, so this is a
//! gated mutation: it refuses without `--yes`, keeping it off the un-gated
//! agent surface. Executes via a direct ssh spawn (no daemon) and streams
//! output as it arrives.

use std::process::ExitCode;

use crate::activity::ActivityKind;
use crate::ssh::{RunEvent, RunHandle, spawn_remote};

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
    let handle = match spawn_remote(&alias, &cmd) {
        Ok(h) => h,
        Err(e) => return fail(&format!("spawn failed: {e}")),
    };
    let exit = drain(handle);
    crate::log_action(ActivityKind::Exec, &alias, "", &cmd, "", Some(exit));
    ExitCode::from(clamp_exit(exit))
}

/// Block on the command's event stream, printing output live. `helm run`
/// can't answer an interactive password prompt (no TTY, no modal), so it
/// flags one and points the operator at `helm shell` instead.
fn drain(mut handle: RunHandle) -> i32 {
    let mut exit = 1;
    while let Ok(ev) = handle.rx.recv() {
        match ev {
            RunEvent::Out(line) => println!("{line}"),
            RunEvent::Err(line) => eprintln!("{line}"),
            RunEvent::Partial(text) => eprint!("{text}"),
            RunEvent::NeedPassword => {
                // No TTY or modal to answer with. Close stdin so the remote
                // `doas`/`sudo` gets EOF and fails fast — otherwise it blocks
                // on the PTY and this loop never sees `Done` (deadlock).
                eprintln!(
                    "\nhelm: password prompt detected — `helm run` can't answer it; \
                     closing input (the command will fail). Use `helm shell open {}` \
                     for interactive auth.",
                    handle.alias
                );
                handle.close_stdin();
            }
            RunEvent::Done(code) => exit = code,
            RunEvent::Error(msg) => {
                eprintln!("helm: {msg}");
                exit = 1;
            }
        }
    }
    exit
}

/// Clamp a raw exit integer (possibly negative from a signal death) into a
/// process exit byte, mirroring `helm exec`'s 130-on-out-of-range rule.
fn clamp_exit(code: i32) -> u8 {
    if (0..=255).contains(&code) {
        code as u8
    } else {
        130
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_exit_maps_out_of_range_to_130() {
        assert_eq!(clamp_exit(0), 0);
        assert_eq!(clamp_exit(1), 1);
        assert_eq!(clamp_exit(255), 255);
        assert_eq!(clamp_exit(-1), 130);
        assert_eq!(clamp_exit(256), 130);
    }
}
