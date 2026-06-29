#![no_main]
//! Fuzz `<alias>[:<label>]` target parsing. For ANY input it must never panic,
//! the alias must never carry the `:` separator (so it can't smuggle a second
//! label into a `-t <target>`), and the session must always be the literal
//! `helm` or a `helm-` prefix — never empty, never attacker-chosen wholesale.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let (alias, session) = helm::tmux::parse_target(s);
        assert!(!alias.contains(':'), "alias leaked a colon: {alias:?}");
        assert!(
            session == "helm" || session.starts_with("helm-"),
            "unexpected session: {session:?}"
        );
    }
});
