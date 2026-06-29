#![no_main]
//! Fuzz the user-editable config parser (~21KB of TOML handling, including the
//! custom `mosh = "auto"|"on"|"off"|bool` deserializer). Malformed input must
//! error cleanly, never panic.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = toml::from_str::<helm::config::Config>(s);
    }
});
