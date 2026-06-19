# helm demo — CLI fleet inspection + the shared agent/human shell
#
# Render with `vhs docs/demo.tape` from the repo root (needs vhs + ttyd and a
# chromium-class browser, which vhs bundles). Output GIF lands at docs/demo.gif
# and is embedded in README.md.
#
# Pre-conditions for re-recording:
#   1. a release build exists — `cargo build --release` (the tape puts
#      ./target/release on $PATH, so it always records the current binary)
#   2. `tmux` is installed (helm shells out to it for the local session)
#
# The recording sandboxes itself under /tmp/helm-vhs-demo: HOME, the tmux
# server, and helm's state dir all point there, so the operator's real config,
# fleet history, and tmux sessions never appear on screen. The only config
# helm loads is the sanitized docs/demo-config.toml.

Output docs/demo.gif

Set Shell bash
Set FontFamily "DejaVu Sans Mono"
Set FontSize 16
Set Width 1100
Set Height 720
Set TypingSpeed 55ms
Set PlaybackSpeed 1.0
Set Padding 16
Set Theme "Catppuccin Mocha"

# --- Bootstrap (hidden): sandbox everything under /tmp/helm-vhs-demo ---
Hide
Type "REPO=$(pwd); SB=/tmp/helm-vhs-demo"
Enter
Type "rm -rf $SB && mkdir -p $SB/state && cp $REPO/docs/demo-config.toml $SB/config.toml"
Enter
Type "export HOME=$SB TMUX_TMPDIR=$SB XDG_STATE_HOME=$SB/state PATH=$REPO/target/release:$PATH PS1='$ '"
Enter
Type "cd $SB"
Enter
# Pre-create the demo session and shrink its pane here in the hidden, soon-to-
# be-cleared bootstrap. `helm shell read` captures the whole pane, and the pane
# inherits the creating terminal's height (~30 rows in this recording), so
# without this a single-line `uptime` read would print a screenful of blank
# rows. The visible `helm shell open` later is idempotent and keeps this size.
Type "helm shell open -d local:demo"
Enter
Type "tmux set-option -t helm-demo window-size manual; tmux resize-window -t helm-demo -x 100 -y 6"
Enter
# Drop the bootstrap's own audit record so `helm activity` later shows only
# the actions the viewer actually saw on screen.
Type "rm -f $SB/state/helm/activity.jsonl"
Enter
Type "clear"
Enter
Show

# --- 1) inspect the fleet ---
Type "# helm inspects your fleet over plain ssh - no agents, no daemons"
Enter
Sleep 800ms
Type "helm ls"
Enter
Sleep 1800ms

Type "helm show web"
Enter
Sleep 1900ms

# --- 2) the shared shell: agent drives, human watches the same tmux pane ---
Type "# a persistent shell the agent drives while the human watches live"
Enter
Sleep 800ms
Type "helm shell open -d local:demo"
Enter
Sleep 1300ms

Type "# read the pane before every send - never type blind"
Enter
Sleep 800ms
Type "helm shell send local:demo 'uptime'"
Enter
Sleep 1300ms
Type "helm shell read local:demo"
Enter
Sleep 2100ms

# --- 3) every action is recorded ---
Type "# exec and shell actions all land in an append-only audit log"
Enter
Sleep 800ms
Type "helm activity -n 5"
Enter
Sleep 2400ms

# --- Teardown (hidden; no Show, so the gif ends on the audit-log frame) ---
Hide
Type "helm shell close local:demo"
Enter
Type "tmux kill-server 2>/dev/null"
Enter
