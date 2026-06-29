#![no_main]
//! Fuzz the crown-jewel shell quoter. For ANY input, `shell_quote` must never
//! panic and must round-trip: an independent POSIX re-parser recovers the exact
//! input from the quoted form. A failure here is a real command-injection CVE —
//! a metacharacter that escaped the quoting would break out of an `ssh <alias>
//! <script>` / `sh -c <script>` slot.
use libfuzzer_sys::fuzz_target;

/// Independent inverse of `shell_quote`: collapse a single shell word built only
/// from single-quoted spans, `\<char>` escapes, and bare literals (exactly the
/// alphabet `shell_quote` emits) back to its logical value. A second
/// implementation, so the round-trip is a true differential.
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

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let quoted = helm::tmux::shell_quote(s);
        assert_eq!(posix_single_unquote(&quoted), s);
    }
});
