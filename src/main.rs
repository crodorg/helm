//! Thin binary entry point. All logic lives in the `helm` library crate
//! (`src/lib.rs`) so the `fuzz/` crate and integration tests can reach the
//! command builders; this shim only forwards to [`helm::run`].

fn main() -> std::process::ExitCode {
    helm::run()
}
