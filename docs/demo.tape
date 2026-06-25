# helm demo — fleet inspection + the in-window pane surface (agent runs, you watch)
#
# Render with `vhs docs/demo.tape` from the repo root (needs vhs + ttyd and a
# chromium-class browser, which vhs bundles). Output GIF lands at docs/demo.gif
# and is embedded in README.md.
#
# Pre-conditions for re-recording:
#   1. a release build exists — `cargo build --release` (the tape puts
#      ./target/release on $PATH, so it always records the current binary)
#   2. `tmux` is installed (helm shells out to it; `helm pane` needs to run
#      *inside* a tmux session, which the bootstrap below sets up)
#
# The recording sandboxes itself under /tmp/helm-vhs-demo: HOME, the tmux
# server, and helm's state dir all point there, so the operator's real config,
# fleet history, hostname, and tmux sessions never appear on screen. The only
# config helm loads is the sanitized docs/demo-config.toml. A throwaway
# ~/.bashrc gives the tmux-spawned panes a clean `$ ` prompt (the default would
# print the real user@host), and the tmux status bar is turned off for the same
# reason.

Output docs/demo.gif

Set Shell bash
Set FontFamily "DejaVu Sans Mono"
Set FontSize 16
Set Width 1100
Set Height 600
Set TypingSpeed 50ms
Set PlaybackSpeed 1.0
Set Padding 16
Set Theme "Catppuccin Mocha"

# --- Bootstrap (hidden): sandbox everything, then enter a clean tmux session ---
Hide
Type "REPO=$(pwd); SB=/tmp/helm-vhs-demo"
Enter
Type "rm -rf $SB && mkdir -p $SB/state && cp $REPO/docs/demo-config.toml $SB/config.toml"
Enter
# A throwaway rc so the prompt is a bare `$ ` (no real user@host on screen).
# A quoted heredoc keeps the single quotes literal; PATH is inherited from the
# exported env below, so the rc only needs to set PS1.
Type "cat > $SB/.bashrc <<'EOF'"
Enter
Type "PS1='$ '"
Enter
Type "EOF"
Enter
Type "cp $SB/.bashrc $SB/.bash_profile"
Enter
Type "export HOME=$SB TMUX_TMPDIR=$SB XDG_STATE_HOME=$SB/state PATH=$REPO/target/release:$PATH"
Enter
# `helm pane` splits a pane in the window helm runs in, so the demo records
# from inside a tmux session. A private TMUX_TMPDIR keeps it off the real server.
Type "cd $SB && tmux new-session -s demo"
Enter
Sleep 800ms
# Status bar off (it shows the hostname); title the anchor pane "agent".
Type "tmux set -g status off; tmux select-pane -T agent"
Enter
Type "clear"
Enter
Show

# --- 1) inspect the fleet (full width, before the split) ---
Type "# helm inspects your fleet over plain ssh - no agents, no daemons"
Enter
Sleep 700ms
Type "helm ls"
Enter
Sleep 2000ms

# Clear the wide fleet table so it doesn't wrap once the pane splits.
Hide
Type "clear"
Enter
Show

# --- 2) the in-window pane: the agent splits a shell in your own tmux ---
Type "# split a drivable shell right in your tmux window"
Enter
Sleep 700ms
Type "helm pane open"
Enter
Sleep 1600ms

# --- 3) one-shot run: output AND exit code in a single call ---
Type "# run it: output AND exit code in one call"
Enter
Sleep 700ms
Type "helm pane run 'uptime'"
Enter
Sleep 1900ms

# --- 4) every action is recorded, local panes included ---
Type "# every action is audited - local panes too"
Enter
Sleep 700ms
Type "helm activity -n 3"
Enter
Sleep 2400ms

# --- Teardown (hidden; no Show, so the gif ends on the audit-log frame) ---
Hide
Type "tmux kill-server 2>/dev/null"
Enter
