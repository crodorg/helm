#!/bin/sh
# qa.sh — hardening / QA runner, layered on top of the `make check` gate.
#
# `make check` stays the canonical commit/push gate (fmt, clippy, test, file-size +
# debt caps, coverage ratchet). This script adds the heavier hardening tools on top:
# dependency audit, fuzzing, mutation testing, memory/UB checks, a command-injection
# semgrep pass, and binary-size analysis. Run subcommands on demand; CI runs
# `qa.sh all` (the cheap, deterministic set).
#
# helm shells out to system ssh/tmux/curl by design, so its center of gravity is
# command injection: the fuzz + mutants + semgrep steps all target the command
# builders (tmux::shell_quote, parse_target, the argv assemblers). helm has no TUI,
# so there is no PTY soak — `stress` is an optional CLI micro-benchmark instead.
#
# Quick start:
#   sh scripts/qa.sh install     # one-time, per machine
#   sh scripts/qa.sh all         # lint + test + audit + semgrep (CI-safe)
#   sh scripts/qa.sh             # show this help
#
# Some subcommands need a nightly toolchain (rustup): fuzz, safety. They detect a
# missing toolchain and tell you what to install rather than failing cryptically.

set -eu

# Run from the crate root regardless of where invoked.
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

FUZZ_SECS="${FUZZ_SECS:-60}"      # per-target fuzz budget (qa.sh fuzz)
BENCH_RUNS="${BENCH_RUNS:-50}"    # hyperfine runs per command (qa.sh stress)

have() { command -v "$1" >/dev/null 2>&1; }

# Require a tool or explain how to get it; returns non-zero (caller decides).
need() {
	if have "$1"; then return 0; fi
	echo "  ! missing: $1 — run: sh scripts/qa.sh install" >&2
	return 1
}

# Require rustup+nightly for the nightly-only tools; guide if absent.
need_nightly() {
	if ! have rustup; then
		echo "  ! no rustup on this box — '$1' needs a nightly toolchain." >&2
		echo "    install rustup, then: rustup toolchain install nightly" >&2
		return 1
	fi
	return 0
}

say() { printf '\n=== %s ===\n' "$1"; }

cmd_install() {
	say "install (global cargo tools)"
	# Stable-toolchain binaries.
	for t in cargo-nextest cargo-deny cargo-machete cargo-audit \
	         cargo-bloat cargo-llvm-lines cargo-mutants cargo-modules; do
		bin="${t#cargo-}"
		if have "$t" || cargo "$bin" --version >/dev/null 2>&1; then
			echo "  ok   $t"
		else
			echo "  --   installing $t"
			cargo install --locked "$t" || echo "  ! $t failed (skipping)"
		fi
	done
	echo
	echo "  Nightly-only tools (need rustup): cargo-fuzz, cargo-careful, miri, sanitizers."
	echo "  With rustup present:"
	echo "    rustup toolchain install nightly"
	echo "    rustup component add miri rust-src --toolchain nightly"
	echo "    cargo install --locked cargo-fuzz cargo-careful"
	echo
	echo "  Non-cargo tools used by some steps:"
	echo "    semgrep   (qa.sh semgrep) — pipx install semgrep   (or: pip install semgrep)"
	echo "    hyperfine (qa.sh stress)  — cargo install hyperfine (or your package manager)"
}

cmd_lint() {
	say "lint (fmt + clippy -D + machete)"
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	if need cargo-machete; then cargo machete; fi
}

cmd_test() {
	say "test"
	if have cargo-nextest; then
		cargo nextest run
	else
		echo "  (cargo-nextest not installed; using cargo test)"
		cargo test
	fi
}

cmd_audit() {
	say "audit (cargo-audit + cargo-deny)"
	if need cargo-audit; then cargo audit; fi
	if need cargo-deny; then
		[ -f deny.toml ] || { echo "  --   no deny.toml; running cargo deny init"; cargo deny init; }
		cargo deny check
	fi
}

cmd_semgrep() {
	say "semgrep (command-injection regression rules)"
	# The rules encode helm's defended invariant: a structural value (alias,
	# session/target label, path) must never reach `sh -c` / `ssh <alias> <str>`
	# as a literal without going through tmux::shell_quote. See etc/semgrep/.
	if ! need semgrep; then
		echo "  (install: pipx install semgrep) — skipping" >&2
		return 0
	fi
	if [ ! -d etc/semgrep ]; then
		echo "  ! no etc/semgrep/ rules yet (added in the W3 security wave)." >&2
		return 0
	fi
	semgrep --error --config etc/semgrep/ src/
}

cmd_fuzz() {
	say "fuzz (cargo-fuzz, ${FUZZ_SECS}s/target)"
	need_nightly fuzz || return 0
	need cargo-fuzz || return 0
	if [ ! -d fuzz ]; then
		echo "  ! no fuzz/ crate yet. The W0 lib split exposes the builders;"
		echo "    add targets with: cargo +nightly fuzz init && cargo +nightly fuzz add <name>"
		echo "    Targets (see hardening.md W2): shell_quote, parse_target, config_toml."
		return 0
	fi
	for t in $(cargo +nightly fuzz list 2>/dev/null); do
		echo "  -- fuzzing $t"
		cargo +nightly fuzz run "$t" -- -max_total_time="$FUZZ_SECS"
	done
}

cmd_mutants() {
	say "mutants (cargo-mutants — grades the test suite)"
	need cargo-mutants || return 0
	# Scope to the command-construction core: tmux.rs (shell_quote, parse_target,
	# runner_cmd) + shell.rs (target/label handling). A surviving mutant here is a
	# quoting/argv invariant the tests don't actually pin — exactly what W2 hardens.
	cargo mutants --file src/tmux.rs --file src/shell.rs
}

cmd_safety() {
	say "safety (miri + sanitizers + careful)"
	need_nightly safety || return 0
	host="$(rustc -vV | sed -n 's/host: //p')"
	# Scope to --lib and skip proptest: property runs are far too slow under
	# interpretation/instrumentation. The pure-logic surface (quoting, parsers,
	# render_*) is what these tools meaningfully check — helm's only `unsafe` is
	# the libc FFI (pty/termios), which these reach via the unit tests.
	echo "  -- miri (lib, skip proptest; disable-isolation for fs/time shims)"
	MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test --lib -- --skip prop \
		|| echo "  ! miri found issues (or needs: rustup component add miri rust-src)"
	echo "  -- AddressSanitizer (lib, build-std; skip proptest)"
	ASAN_OPTIONS="detect_leaks=0:detect_odr_violation=0" RUSTFLAGS="-Zsanitizer=address" \
		cargo +nightly test --lib -Zbuild-std --target "$host" -- --skip prop \
		|| echo "  ! ASan run failed/flagged"
	echo "  -- ThreadSanitizer (lib, build-std; skip proptest)"
	RUSTFLAGS="-Zsanitizer=thread" \
		cargo +nightly test --lib -Zbuild-std --target "$host" -- --skip prop \
		|| echo "  ! TSan run failed/flagged"
	if have cargo-careful; then
		echo "  -- cargo-careful (full suite)"; cargo +nightly careful test || echo "  ! careful flagged"
	fi
}

cmd_stress() {
	say "stress (optional CLI micro-bench — helm has no TUI to soak)"
	# The TUI→CLI refactor removed the only long-running process, so there is no
	# PTY soak / idle-CPU wave. This is a light cold-start + hot-read benchmark
	# instead; purely informational, never gated.
	if ! need hyperfine; then
		echo "  N/A — no TUI soak; install hyperfine for the optional CLI bench." >&2
		return 0
	fi
	cargo build --release
	bin="$ROOT/target/release/helm"
	echo "  -- hyperfine: cold start (helm --version) + help render"
	hyperfine --warmup 3 --runs "$BENCH_RUNS" \
		"$bin --version" \
		"$bin --help"
}

cmd_min() {
	say "min (bloat + llvm-lines + modules)"
	if need cargo-bloat; then cargo bloat --release --crates | head -25; fi
	if need cargo-llvm-lines; then cargo llvm-lines | head -25; fi
	if need cargo-modules; then
		cargo modules structure 2>/dev/null || cargo modules generate tree 2>/dev/null \
			|| echo "  ! cargo-modules CLI shape differs; run it manually"
	fi
}

cmd_gate() {
	say "gate (delegating to make check — the canonical project gate)"
	make check
}

cmd_all() {
	# CI-safe set: the project gate + dependency audit + the semgrep regression
	# pass. Long/interactive tools (fuzz, mutants, safety, stress) run on demand.
	cmd_gate
	cmd_audit
	cmd_semgrep
}

usage() {
	cat <<'EOF'
qa.sh — hardening / QA runner (layered on `make check`)

  install   cargo install the global tools (once per machine)
  lint      fmt --check + clippy -D + cargo-machete
  test      cargo-nextest run (falls back to cargo test)
  audit     cargo-audit + cargo-deny                  [security: supply chain]
  semgrep   command-injection regression rules        [security: injection]
  fuzz      run cargo-fuzz targets (FUZZ_SECS=60)      [security: input surface]
  mutants   cargo-mutants on tmux.rs + shell.rs — grade the injection tests
  safety    miri + sanitizers + cargo-careful          [security: memory/UB]
  stress    optional hyperfine CLI bench (no TUI to soak)
  min       cargo-bloat + llvm-lines + modules
  gate      make check (the existing commit/push gate)
  all       CI-safe: gate + audit + semgrep

  fuzz/safety need rustup+nightly; safety's sanitizers also need rust-src
  (rustup component add rust-src --toolchain nightly) for -Zbuild-std.
  semgrep needs the semgrep CLI (pipx install semgrep).
EOF
}

case "${1:-help}" in
	install) cmd_install ;;
	lint)    cmd_lint ;;
	test)    cmd_test ;;
	audit)   cmd_audit ;;
	semgrep) cmd_semgrep ;;
	fuzz)    cmd_fuzz ;;
	mutants) cmd_mutants ;;
	safety)  cmd_safety ;;
	stress)  cmd_stress ;;
	min)     cmd_min ;;
	gate)    cmd_gate ;;
	all)     cmd_all ;;
	help|-h|--help) usage ;;
	*) echo "unknown subcommand: $1" >&2; usage; exit 2 ;;
esac
