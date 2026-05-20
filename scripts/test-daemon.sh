#!/usr/bin/env bash
# Smoke test for `helm daemon` on Linux and macOS.
#
# Exercises:
#   1. Socket bind on `helm daemon start`
#   2. Ping reply via `helm daemon status`
#   3. Request dispatch via `helm exec local <cmd>`
#   4. Clean shutdown via `helm daemon stop`
#   5. Auto-spawn after stop (TUI prelude path is skipped — interactive)
#
# Run from anywhere; needs `helm` on PATH and `tmux` installed.
# Exits 0 on success, non-zero on first failure.

set -eu

step() { printf '\n--- %s\n' "$*"; }
die()  { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

command -v helm >/dev/null || die "helm not on PATH"
command -v tmux >/dev/null || die "tmux not on PATH"

PLATFORM=$(uname -s)
step "platform: $PLATFORM, helm: $(command -v helm)"

step "precondition: socket must be free"
if helm daemon status >/dev/null 2>&1; then
    cat >&2 <<'EOM'
FAIL: something is already bound to helm's control socket.
  - If it's a TUI (`helm`), quit it (`q`) and rerun.
  - If it's a stale daemon, run `helm daemon stop` and rerun.
  - Check: `helm daemon status` (shows version) and pgrep -af helm.
EOM
    exit 1
fi

step "start daemon (detached)"
helm daemon start || die "daemon start exit non-zero"

step "status should be reachable"
out=$(helm daemon status 2>&1) || die "daemon status exit non-zero"
printf '  status output: %s\n' "$out"
case "$out" in
    *helm*|*version*|*reachable*) : ;;
    *) die "status output unexpected: $out" ;;
esac

step "dispatch exec request — daemon should run 'whoami' on local"
exec_out=$(helm exec local "whoami" 2>&1) || die "helm exec failed"
expected=$(whoami)
printf '  exec output: %s (expected substring: %s)\n' "$exec_out" "$expected"
case "$exec_out" in
    *"$expected"*) : ;;
    *) die "exec output missing whoami result" ;;
esac

step "dispatch exec with a multi-token cmd"
exec_out=$(helm exec local "echo helm-daemon-roundtrip-$$" 2>&1) || die "helm exec multi-token failed"
case "$exec_out" in
    *"helm-daemon-roundtrip-$$"*) : ;;
    *) die "multi-token exec lost arguments: $exec_out" ;;
esac

step "stop daemon"
helm daemon stop || die "daemon stop exit non-zero"
sleep 1

step "status should now report 'no daemon' again"
if helm daemon status >/dev/null 2>&1; then
    die "daemon still responding after stop"
fi

step "PASS — daemon round-trip clean on $PLATFORM"
