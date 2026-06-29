//! Thin shell-out wrappers around tmux, either via ssh to a remote host or
//! against the operator's local tmux server.
//!
//! Sessions live on the chosen host. Each `target` parses as `alias[:label]`:
//!
//! - `vps1`         → ssh to `vps1`, tmux session name `helm`
//! - `vps1:deploy`  → ssh to `vps1`, tmux session name `helm-deploy`
//! - `local`        → operator's machine, tmux session name `helm`
//! - `local:agent`  → operator's machine, tmux session name `helm-agent`
//!
//! The reserved alias `local` short-circuits ssh and runs tmux directly on
//! the operator's machine. Use cases: a shell that needs interactive doas/sudo
//! password entry, or a separate command history from the operator's own
//! shell. Each ssh'd host has its own tmux server; locally there's one.
//! Session names don't need to include the alias for uniqueness — the
//! alias picks the server, the label namespaces sessions within it.
//!
//! Sessions are created with `tmux new-session -A` (attach-or-create),
//! which is idempotent. `ensure_session` is a single fire-and-forget call.
//!
//! Why remote-tmux: persistence survives helm restarts AND network drops
//! AND operator-machine reboots. A remote session lives on the VPS until
//! the VPS itself reboots or someone runs `tmux kill-server` on it. A
//! `local` session lives until the operator's machine reboots or its
//! tmux server is killed.
//!
//! Text passed to `send_keys` is quoted for POSIX shell evaluation, so
//! passwords / shell metachars / spaces survive the round-trip.

use anyhow::{Context, Result, anyhow};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// Default scrollback lines for `read` (the agent-facing capture). Kept low
/// because a headless helm pane is 50 rows tall and a routine read is mostly
/// blank padding — see `runcmd::strip_trailing_blank`. Long logs pass an
/// explicit `-n`.
pub const DEFAULT_CAPTURE_LINES: u32 = 200;

/// Reserved alias meaning "run tmux on the operator's machine, not via ssh".
pub const LOCAL_ALIAS: &str = "local";

/// Process-global flags inserted after `tmux` on every invocation helm makes
/// (e.g. `-u` to force UTF-8). Set once at startup from config via
/// [`set_flags`]; unset means plain `tmux`. A global keeps the tmux helpers'
/// signatures unchanged — config is loaded once per process, so a single
/// set is enough for the CLI, the TUI, and the daemon alike.
static TMUX_FLAGS: OnceLock<Vec<String>> = OnceLock::new();

/// Install the global tmux flags. First call wins (OnceLock); later calls are
/// no-ops, which is fine since each helm process loads config exactly once.
pub fn set_flags(flags: Vec<String>) {
    let _ = TMUX_FLAGS.set(flags);
}

/// The configured flags, or an empty slice if [`set_flags`] was never called.
/// Used by the local attach path, which builds a `Command` and pushes args
/// directly rather than embedding a shell script.
pub fn flags() -> Vec<String> {
    TMUX_FLAGS.get().cloned().unwrap_or_default()
}

/// The `tmux` command word plus configured global flags, shell-quoted and
/// space-joined for embedding in a remote script — e.g. `tmux -u`. Split out
/// as a pure helper for testing the global-free path.
fn build_tmux_prefix(flags: &[String]) -> String {
    let mut s = String::from("tmux");
    for f in flags {
        s.push(' ');
        s.push_str(&shell_quote(f));
    }
    s
}

/// `tmux` with the process-global flags applied (see [`set_flags`]).
pub fn tmux_prefix() -> String {
    build_tmux_prefix(TMUX_FLAGS.get().map(Vec::as_slice).unwrap_or(&[]))
}

/// Parse `alias[:label]` into `(alias, remote_session_name)`.
pub fn parse_target(target: &str) -> (String, String) {
    match target.split_once(':') {
        Some((alias, label)) if !label.is_empty() => (alias.to_string(), format!("helm-{label}")),
        Some((alias, _)) => (alias.to_string(), "helm".to_string()),
        None => (target.to_string(), "helm".to_string()),
    }
}

/// POSIX-shell-quote a string so it survives ssh's single round of remote
/// shell evaluation intact. Cheap allocation for the common case (typical
/// shell commands), but produces correct output for any UTF-8 input.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_=.,:@".contains(&b))
    {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Prefix common Homebrew + MacPorts directories onto `$PATH` for the
/// remote (or local-via-`sh -c`) shell. Non-interactive ssh sessions on
/// macOS skip `.zshrc` and never pick up `/opt/homebrew/bin`, so a stock
/// `ssh <laptop> tmux ...` fails with `tmux: command not found` even after
/// `brew install tmux`. Uses `export` (not the `KEY=value cmd` prefix
/// form) so compound scripts like `tmux has-session || tmux new-session`
/// see the augmented PATH on every branch. Idempotent on hosts that
/// already have these dirs in `PATH`.
pub fn with_remote_path(script: &str) -> String {
    format!("export PATH=\"$PATH:/opt/homebrew/bin:/usr/local/bin:/opt/local/bin\"; {script}")
}

/// Build the command that runs `script` on the host identified by `alias`.
/// For real ssh aliases this is `ssh <alias> -- <script>`; for the reserved
/// `local` alias it's `sh -c <script>`, skipping ssh entirely. The script
/// is always wrapped with `with_remote_path` so brew-installed tools
/// resolve even in non-interactive ssh shells.
pub(crate) fn runner_cmd(alias: &str, script: &str) -> Command {
    let script = with_remote_path(script);
    if alias == LOCAL_ALIAS {
        let mut c = Command::new("sh");
        c.arg("-c").arg(script);
        c
    } else {
        let mut c = Command::new("ssh");
        // `--` ends ssh option parsing so a `-`-leading alias can't be read as
        // an ssh flag (e.g. `-oProxyCommand=…`). See `ssh::run::spawn_remote`.
        c.arg("--").arg(alias).arg(script);
        c
    }
}

/// Create the session detached if it doesn't already exist. `-A` makes
/// new-session attach-or-create, and `-d` keeps it detached so we don't
/// hijack stdin/stdout.
pub fn ensure_session(target: &str) -> Result<()> {
    let (alias, session) = parse_target(target);
    // `has-session || new-session -d` is the tty-less idempotent create.
    // The `-A` flag would be simpler but with `-d` it triggers tmux's
    // detach-client path, which needs a tty and fails over ssh with
    // `open terminal failed: not a terminal`. `-x`/`-y` set a default
    // pane size; the operator's later attach resizes to their real
    // terminal automatically.
    let q = shell_quote(&session);
    let tmux = tmux_prefix();
    let remote = format!(
        "{tmux} has-session -t {q} 2>/dev/null || {tmux} new-session -d -x 200 -y 50 -s {q}"
    );
    let status = runner_cmd(&alias, &remote)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("spawn tmux new-session runner")?;
    if !status.success() {
        return Err(anyhow!(
            "`tmux new-session` on {alias} failed (exit {})",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Build the remote script that attach-or-creates the session for an
/// interactive `helm shell open` (used by both the ssh and mosh transports).
/// `-A` makes `new-session` idempotent (attach if it exists, else create).
///
/// The session name is **shell-quoted**: it embeds `helm-<label>` where
/// `<label>` is the user-controlled half of an `<alias>:<label>` target, and
/// this script is handed to a remote shell as a single `ssh <alias> <script>`
/// (or `mosh … sh -c <script>`) string. Without quoting, a label containing a
/// space or shell metacharacter (`;`, `|`, `$`, …) would break out of its slot
/// — a command injection. Mirrors the quoting every other tmux helper applies
/// (`ensure_session`, `kill`, `send_keys`).
pub(crate) fn attach_session_script(session: &str) -> String {
    with_remote_path(&format!(
        "{} new-session -A -s {}",
        tmux_prefix(),
        shell_quote(session)
    ))
}

/// Send a line of text (followed by Enter) to the session's active pane.
/// Text is split into two `send-keys` calls — first `-l` (literal, no
/// key-name parsing) for the body, then a bare `Enter`. This avoids every
/// escaping pitfall with characters tmux would otherwise interpret as key
/// names (e.g. text containing the word `Space` or `Enter`).
pub fn send_keys(target: &str, text: &str) -> Result<()> {
    let (alias, session) = parse_target(target);
    let q_session = shell_quote(&session);
    let q_text = shell_quote(text);
    let tmux = tmux_prefix();
    // `--` ends tmux's option parsing so a body starting with `-` (e.g.
    // `--help`) is treated as literal keys, not a `send-keys` flag.
    let remote = format!(
        "{tmux} send-keys -t {q_session} -l -- {q_text} && {tmux} send-keys -t {q_session} Enter"
    );
    let status = runner_cmd(&alias, &remote)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("spawn tmux send-keys runner")?;
    if !status.success() {
        return Err(anyhow!(
            "`tmux send-keys` on {alias} failed (exit {})",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Send raw tmux key specs to the session's active pane — no literal `-l`
/// flag and no trailing Enter. This is the TUI-driving path: each `key` is a
/// tmux key name (`Up`, `C-c`, `Enter`, `Escape`, `F1`, …) passed straight
/// through, so the agent can operate a full-screen program (vim, htop, a
/// menu) on a remote host, which the line-oriented `send_keys` cannot do.
/// `--` ends option parsing so a key spec beginning with `-` is never
/// mistaken for a `send-keys` flag.
pub fn send_raw_keys(target: &str, keys: &[String]) -> Result<()> {
    if keys.is_empty() {
        return Err(anyhow!("no keys to send"));
    }
    let (alias, session) = parse_target(target);
    let q_session = shell_quote(&session);
    let joined = keys
        .iter()
        .map(|k| shell_quote(k))
        .collect::<Vec<_>>()
        .join(" ");
    let remote = format!("{} send-keys -t {q_session} -- {joined}", tmux_prefix());
    let status = runner_cmd(&alias, &remote)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("spawn tmux send-keys (raw) runner")?;
    if !status.success() {
        return Err(anyhow!(
            "`tmux send-keys` (raw) on {alias} failed (exit {})",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Capture the active pane's contents from the session.
pub fn capture(target: &str, lines: u32) -> Result<String> {
    let (alias, session) = parse_target(target);
    let q_session = shell_quote(&session);
    let neg = format!("-{lines}");
    let q_neg = shell_quote(&neg);
    let remote = format!(
        "{} capture-pane -t {q_session} -p -S {q_neg}",
        tmux_prefix()
    );
    let out = runner_cmd(&alias, &remote)
        .output()
        .context("spawn tmux capture-pane runner")?;
    if !out.status.success() {
        return Err(anyhow!(
            "`tmux capture-pane` on {alias} failed (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// List helm-* sessions on the given alias's tmux server. Returns the
/// user-facing target form for each (e.g. `vps1`, `vps1:deploy`, `local`,
/// `local:agent`).
pub fn list(alias: &str) -> Result<Vec<String>> {
    let remote = format!(
        "{} list-sessions -F '#{{session_name}}' 2>/dev/null || true",
        tmux_prefix()
    );
    let out = runner_cmd(alias, &remote)
        .output()
        .context("spawn tmux list-sessions runner")?;
    if !out.status.success() {
        return Err(anyhow!(
            "tmux list-sessions on {alias} failed (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut targets = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let session = line.trim();
        if session == "helm" {
            targets.push(alias.to_string());
        } else if let Some(label) = session.strip_prefix("helm-") {
            targets.push(format!("{alias}:{label}"));
        }
    }
    Ok(targets)
}

/// Kill the session for `target`.
pub fn kill(target: &str) -> Result<()> {
    let (alias, session) = parse_target(target);
    let remote = format!(
        "{} kill-session -t {}",
        tmux_prefix(),
        shell_quote(&session)
    );
    let status = runner_cmd(&alias, &remote)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("spawn tmux kill-session runner")?;
    if !status.success() {
        return Err(anyhow!(
            "`tmux kill-session` on {alias} failed (exit {})",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_no_label_uses_default_session() {
        assert_eq!(parse_target("vps1"), ("vps1".into(), "helm".into()));
    }

    #[test]
    fn target_with_label_prefixes_helm() {
        assert_eq!(
            parse_target("vps1:deploy"),
            ("vps1".into(), "helm-deploy".into())
        );
    }

    #[test]
    fn target_with_empty_label_uses_default() {
        // `alias:` is treated as no label.
        assert_eq!(parse_target("vps1:"), ("vps1".into(), "helm".into()));
    }

    #[test]
    fn target_with_multiple_colons_takes_first_split() {
        // `alias:a:b` → label is `a:b`, session name `helm-a:b`. tmux
        // session names containing `:` are fine when properly quoted.
        assert_eq!(parse_target("vps1:a:b"), ("vps1".into(), "helm-a:b".into()));
    }

    #[test]
    fn shell_quote_passes_through_safe_strings() {
        assert_eq!(shell_quote("helm"), "helm");
        assert_eq!(shell_quote("uptime"), "uptime");
        assert_eq!(shell_quote("/var/log/messages"), "/var/log/messages");
        assert_eq!(shell_quote("helm-deploy"), "helm-deploy");
    }

    #[test]
    fn shell_quote_wraps_text_with_spaces_or_metachars() {
        assert_eq!(shell_quote("hello world"), "'hello world'");
        assert_eq!(
            shell_quote("doas rcctl restart httpd"),
            "'doas rcctl restart httpd'"
        );
        assert_eq!(shell_quote("$VAR | rm -rf /"), "'$VAR | rm -rf /'");
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("'"), "''\\'''");
    }

    #[test]
    fn shell_quote_handles_empty() {
        assert_eq!(shell_quote(""), "''");
    }

    fn cmd_program(c: &Command) -> String {
        c.get_program().to_string_lossy().into_owned()
    }

    fn cmd_args(c: &Command) -> Vec<String> {
        c.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn runner_cmd_local_uses_sh_dash_c() {
        let c = runner_cmd(LOCAL_ALIAS, "tmux has-session -t helm");
        assert_eq!(cmd_program(&c), "sh");
        let args = cmd_args(&c);
        assert_eq!(args[0], "-c");
        assert!(args[1].contains("tmux has-session -t helm"));
        assert!(args[1].contains("/opt/homebrew/bin"));
    }

    #[test]
    fn runner_cmd_remote_uses_ssh() {
        let c = runner_cmd("vps1", "tmux has-session -t helm");
        assert_eq!(cmd_program(&c), "ssh");
        let args = cmd_args(&c);
        // `--` ends ssh option parsing before the alias (W3 hardening).
        assert_eq!(args[0], "--");
        assert_eq!(args[1], "vps1");
        assert!(args[2].contains("tmux has-session -t helm"));
        assert!(args[2].contains("/opt/homebrew/bin"));
    }

    #[test]
    fn build_tmux_prefix_empty_is_bare_tmux() {
        assert_eq!(build_tmux_prefix(&[]), "tmux");
    }

    #[test]
    fn build_tmux_prefix_appends_flags_shell_quoted() {
        assert_eq!(build_tmux_prefix(&["-u".into()]), "tmux -u");
        assert_eq!(build_tmux_prefix(&["-u".into(), "-2".into()]), "tmux -u -2");
        // A flag with a space/metachar gets quoted so it survives the
        // remote shell intact.
        assert_eq!(
            build_tmux_prefix(&["-L".into(), "my socket".into()]),
            "tmux -L 'my socket'"
        );
    }

    #[test]
    fn with_remote_path_exports_before_script() {
        let s = with_remote_path("tmux a || tmux b");
        // `export` (not the `KEY=val cmd` prefix form) so both branches
        // of a compound script inherit the augmented PATH.
        assert!(s.starts_with("export PATH="));
        assert!(s.contains("/opt/homebrew/bin"));
        assert!(s.contains("tmux a || tmux b"));
        assert!(s.contains("; tmux a"));
    }

    // --- Command-injection regression test for the attach path ---

    #[test]
    fn attach_session_script_quotes_a_session_with_metachars() {
        // Regression (hardening W2): the `helm shell open` remote-attach path
        // used to interpolate the session (`helm-<label>`, user-controlled)
        // raw, so a label with a space or `;` broke out of the
        // `ssh <alias> <script>` string. The session must be shell-quoted.
        let session = "helm-; touch /tmp/pwned";
        let script = attach_session_script(session);
        // The dangerous session appears only inside its quoted form…
        assert!(script.contains("-s 'helm-; touch /tmp/pwned'"), "{script}");
        // …never as a bare `-s helm-; …` that the remote shell would split on
        // the `;` and run `touch /tmp/pwned` as a separate command.
        assert!(!script.contains("-s helm-; touch"), "{script}");
    }

    // --- Property tests (names contain `prop` so `qa.sh safety` can --skip them
    //     under the slow miri/sanitizer runs) ---

    use proptest::prelude::*;

    /// An independent POSIX re-parser: collapse a single shell *word* built
    /// only from single-quoted spans, `\<char>` escapes, and bare literals
    /// (exactly the alphabet `shell_quote` emits) back to its logical value.
    /// A second implementation of the inverse, so the round-trip check is a
    /// real differential, not `shell_quote` checking itself.
    fn posix_single_unquote(quoted: &str) -> String {
        let mut out = String::new();
        let mut chars = quoted.chars();
        let mut in_single = false;
        while let Some(c) = chars.next() {
            if in_single {
                if c == '\'' {
                    in_single = false;
                } else {
                    out.push(c);
                }
            } else {
                match c {
                    '\'' => in_single = true,
                    '\\' => {
                        if let Some(n) = chars.next() {
                            out.push(n);
                        }
                    }
                    _ => out.push(c),
                }
            }
        }
        out
    }

    /// Strings biased toward shell-significant bytes (plus a few normal and
    /// non-ASCII chars), excluding NUL (which can't appear in an argv element).
    fn shellish() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                Just(' '),
                Just('\t'),
                Just('\n'),
                Just(';'),
                Just('|'),
                Just('&'),
                Just('$'),
                Just('`'),
                Just('\''),
                Just('"'),
                Just('\\'),
                Just('('),
                Just(')'),
                Just('<'),
                Just('>'),
                Just('*'),
                Just('?'),
                Just('!'),
                Just('#'),
                Just('~'),
                Just('='),
                Just('a'),
                Just('Z'),
                Just('0'),
                Just('/'),
                Just('-'),
                Just('é'),
                Just('→'),
            ],
            0..24,
        )
        .prop_map(|cs| cs.into_iter().collect())
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 1024, ..ProptestConfig::default() })]
        /// The crown jewel: for ANY input, the pure re-parser recovers it
        /// exactly from `shell_quote`'s output — no metacharacter escapes the
        /// quoting. Pure (no subprocess), so run it hard.
        #[test]
        fn prop_shell_quote_roundtrips_pure(s in shellish()) {
            prop_assert_eq!(posix_single_unquote(&shell_quote(&s)), s);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]
        /// Differential against a REAL POSIX shell: `printf %s <quoted>` must
        /// echo the input verbatim. The hardened invariant from the plan — a
        /// failure here would be an actual injection CVE. Bounded case count
        /// because each case spawns `sh`.
        #[test]
        fn prop_shell_quote_roundtrips_via_sh(s in shellish()) {
            let script = format!("printf %s {}", shell_quote(&s));
            let out = Command::new("sh")
                .arg("-c")
                .arg(&script)
                .output()
                .expect("spawn sh");
            prop_assert!(out.status.success());
            prop_assert_eq!(out.stdout, s.into_bytes());
        }
    }

    proptest! {
        /// `parse_target` structural invariants: the alias never carries the
        /// `:` separator (so it can't smuggle a second label), and the session
        /// is always the literal `helm` or a `helm-` prefix — never empty,
        /// never attacker-chosen wholesale.
        #[test]
        fn prop_parse_target_invariants(target in shellish()) {
            let (alias, session) = parse_target(&target);
            prop_assert!(!alias.contains(':'), "alias leaked a colon: {:?}", alias);
            prop_assert!(session == "helm" || session.starts_with("helm-"), "{session}");
        }
    }

    proptest! {
        /// `runner_cmd` never lets a script (however hostile) inject an extra
        /// argv element: it is always exactly `sh -c <script>` or
        /// `ssh <alias> <script>` — two args, the script delivered verbatim as
        /// one element (shell interpretation happens later, by design).
        #[test]
        fn prop_runner_cmd_never_splits_script(alias in shellish(), script in shellish()) {
            let c = runner_cmd(&alias, &script);
            let args = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
            let expected = with_remote_path(&script);
            if alias == LOCAL_ALIAS {
                prop_assert_eq!(cmd_program(&c), "sh");
                prop_assert_eq!(args, vec!["-c".to_string(), expected]);
            } else {
                prop_assert_eq!(cmd_program(&c), "ssh");
                // `--` ends ssh option parsing before the alias (W3 hardening),
                // so even a `-`-leading alias is delivered as a destination, not
                // an ssh flag — and the script is still exactly one arg.
                prop_assert_eq!(args, vec!["--".to_string(), alias, expected]);
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]
        /// The fixed attach path: for ANY label, the script delivers exactly
        /// `helm-<label>` to tmux's `-s` as a single shell word — the label can
        /// never break out. Verified differentially through a real shell by
        /// standing in `printf` for the `tmux …` head of the script.
        #[test]
        fn prop_attach_session_label_cannot_break_out(label in shellish()) {
            let (_, session) = parse_target(&format!("vps1:{label}"));
            let script = attach_session_script(&session);
            // Strip the `export PATH=…; ` prefix, then stand `printf '%s\0'` in
            // for the `tmux` head so the shell prints exactly the args tmux
            // would have received — proving how `-s <session>` tokenizes. A NUL
            // delimiter keeps a session that itself contains a newline intact.
            let body = script.strip_prefix(&with_remote_path("")).unwrap();
            let printf_body = body.replacen(&tmux_prefix(), "printf '%s\\0'", 1);
            let out = Command::new("sh")
                .arg("-c")
                .arg(&printf_body)
                .output()
                .expect("spawn sh");
            prop_assert!(out.status.success());
            // Split on NUL; printf leaves a trailing NUL, so drop the final "".
            let mut toks: Vec<String> = out
                .stdout
                .split(|&b| b == 0)
                .map(|c| String::from_utf8_lossy(c).into_owned())
                .collect();
            prop_assert_eq!(toks.last().map(String::as_str), Some(""));
            toks.pop();
            // tmux would see: new-session -A -s <session> — and <session> is a
            // single token equal to the intended name, never split or extended.
            prop_assert_eq!(toks, vec![
                "new-session".to_string(),
                "-A".to_string(),
                "-s".to_string(),
                session,
            ]);
        }
    }
}
