# helm demo — sessions pane + shell open round-trip
#
# Render with `vhs docs/demo.tape` (requires vhs, ttyd, and a chromium-class
# browser; install via `brew install vhs` on macOS — bundles its own
# Chromium via Playwright).
#
# Output GIF lands at docs/demo.gif and is referenced from README.md.
#
# Pre-conditions for re-recording:
#   1. `helm` is on $PATH (the symlink at ~/bin/helm works)
#   2. ssh-agent is loaded with whichever keys your `config.toml` references
#   3. The `mac` (or whichever) Host alias in ~/.ssh/config is reachable
#   4. Run `helm daemon stop` first if anything else is using the socket

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

# Pre-seed two `local` sessions so the sessions pane has rows to show.
Hide
Type "helm shell open -d local:edit"
Enter
Sleep 200ms
Type "helm shell open -d local:tail"
Enter
Sleep 200ms
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
