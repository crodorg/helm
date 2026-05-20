# helm demo — sessions pane + shell open round-trip
#
# Render with `vhs docs/demo.tape` (requires vhs, ttyd, and a chromium-class
# browser; `xbps-install vhs ttyd` on Void; `brew install vhs` on macOS
# bundles Playwright Chromium).
#
# Output GIF lands at docs/demo.gif and is referenced from README.md.
#
# Pre-conditions for re-recording:
#   1. `helm` is on $PATH (the symlink at ~/bin/helm works)
#   2. `tmux` is installed (helm shells out to it for the local sessions)
#
# The recording sandboxes itself in /tmp/helm-vhs-demo so the operator's
# real ~/.config/helm/config.toml never appears on screen. The sanitized
# config at docs/demo-config.toml is what helm actually loads during the
# recording.

Output docs/demo.gif

Set Shell bash
Set FontFamily "Monaspace Neon"
Set FontSize 16
Set Width  1400
Set Height 800
Set TypingSpeed 55ms
Set PlaybackSpeed 1.0
Set Padding 16
Set Theme "Catppuccin Mocha"

# Bootstrap: cd into a sandbox holding only the sanitized demo config, and
# point tmux at an isolated server socket so the operator's real tmux
# sessions never appear in the GIF.
Hide
Type "REPO=$(pwd) && mkdir -p /tmp/helm-vhs-demo && cp $REPO/docs/demo-config.toml /tmp/helm-vhs-demo/config.toml && cd /tmp/helm-vhs-demo && export TMUX_TMPDIR=/tmp/helm-vhs-demo"
Enter
Sleep 300ms
Type "helm shell open -d local:edit"
Enter
Sleep 400ms
Type "helm shell open -d local:tail"
Enter
Sleep 400ms
Type "clear"
Enter
Sleep 200ms
Show

# Open the TUI.
Type "helm"
Enter
Sleep 2500ms

# Drop into the sessions pane.
Type "S"
Sleep 2500ms

# Cursor walk.
Down
Sleep 400ms
Down
Sleep 400ms
Up
Sleep 400ms

# Show a detached-ensure (creates a session in place without leaving TUI).
Type "d"
Sleep 1200ms

# Back to Browse and quit.
Escape
Sleep 800ms
Type "q"
Sleep 800ms
