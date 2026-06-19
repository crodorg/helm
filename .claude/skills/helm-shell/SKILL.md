---
name: helm-shell
description: "Drive a persistent shell the operator and I both see live — a helm tmux session on a remote device (ssh alias), or a local pane split in the operator's current tmux window (reserved target `here`). Remote work auto-opens a read-only viewport pane in their window (opt out: 'headless'); sessions survive disconnects; remote targets driven via `helm shell` CLI subcommands, `here` panes via raw tmux. MANDATORY TRIGGERS: 'send to helm', 'put this in the shell', 'run in my helm shell', 'watch me run X via helm', 'use helm-shell', 'spawn a local shell', 'in a local tmux', 'run it here', 'open a pane here', 'in a pane in this window', 'split a pane here', 'open a viewport', 'watch <host> in a pane'. Also use when a helm shell is already open on the target host and the request is shell-stateful (cwd/env/history), or when a command needs interactive doas/sudo password entry. SKIP one-shot stateless requests — `helm exec` or direct ssh is simpler."
---

# helm-shell

A bridge skill. The operator runs a persistent tmux session on a chosen host (via `helm shell`). They watch it live by attaching from any terminal. I drive it from the side: read its scrollback, type commands, never blind. The operator can intervene at any moment — passwords, Ctrl-C, anything — because it is their tmux session.

The host is selected by the alias in `<target>` — and the host decides the transport:

- A real ssh alias (e.g. `web`, `vps1`) → tmux runs on that device over ssh. This is the ONLY way I drive another device — never an ssh process inside a local pane I send keys to. By default the session gets a read-only **viewport pane** in the operator's current window so they watch live (see *viewport panes* below).
- The reserved target `here` → not a session at all: a **pane split inside the tmux window I'm running in**, on the operator's own tmux session — a local shell, this machine only. See *the `here` target* below — it is the one target driven via raw `tmux`, not the helm CLI.
- The reserved alias `local` → escape hatch, not part of normal routing: a separate tmux session on the operator's machine for shells that must outlive the current window. See *the `local` escape hatch* below.

This is fundamentally different from `helm exec`:

- `helm exec` = one-shot, output streams back to my conversation, no shell state retained.
- `helm shell` = stateful long-lived shell, output renders in the operator's tmux pane (and I can scrape it), shell state (cwd, env, history, in-progress prompts) persists.

Use `helm shell` when the operator wants to *watch* me work or when shell state matters. Use `helm exec` otherwise.

Two sibling surfaces need no shell at all:

- **Read verbs** — a quick read of fleet state: `helm ls`, `show <host>`, `svc <host>`, `ps <host>`, `ports <host>`, `health`, `dns`, `vultr`, `money`, `logs <host>`, `history`, `activity` (add `--json` for machine output). Lighter than opening a shell just to eyeball state.
- **Mutating verbs** are operator-only: `helm vultr {reboot,halt,start,snapshot} <id>` and `helm run <key> <host>` refuse without `--yes` and sit off my un-gated surface. I never invoke them — if a mutation is wanted, I narrate intent and let the operator run it.

---

## routing: which target a request means

The operator lives inside tmux, so every request could plausibly be "a pane here." The differentiator is **where the shell process runs**, never where it's viewed:

| operator says | shell runs | transport | operator watches via |
|---|---|---|---|
| names a device (`web`, `vps1`, `relay`, …) | that device's tmux | `helm shell` over ssh — always | auto-opened viewport pane here |
| names a device + "headless" | that device's tmux | `helm shell` over ssh | attaches when they want (`helm open <alias>`) |
| "here", "a pane", "a local shell", or no device named | this machine | raw tmux pane in my window | the pane itself |
| explicitly `local`, or "outlive the window" | this machine, separate session | `helm shell`, no ssh | see *the `local` escape hatch* |

- **`here` always means this machine.** If we're mid-conversation about a device and the operator says "run it here," ask which they mean — never guess between the here pane and the device.
- **One transport per shell.** Anything with a host goes through the helm CLI; raw tmux drives only local panes in my own window. A viewport pane showing a remote session is read-only to me — raw send-keys into it is banned.

---

## the four primitives

All commands are invoked via the Bash tool.

```sh
helm shell list <alias>                    # which helm-* sessions exist on the host (alias or `local`)
helm shell open -d <target>                # create the session detached, no attach
helm shell read <target> [-n LINES]        # scrape scrollback (default 1000)
helm shell send <target> "<text>"          # type a line (auto-Enter)
```

`<target>` is `<alias>` (default session `helm` on that host) or `<alias>:<label>` (session `helm-<label>`). `<alias>` is a real ssh host (or the escape-hatch `local` — see *the `local` escape hatch*).

The operator-facing command is `helm shell open <target>` — that attaches *their* terminal. From my Bash tool I only call the headless variants (`read`, `send`, `open -d`); the bare attaching `open` appears in exactly two places: as a viewport pane's command (the default for remote work — see *viewport panes*) and in the opt-in self-attach flow below. I never attach a terminal for myself.

---

## target fidelity: open exactly what the operator named

When the operator names a target, that exact target is what I act on. Two failure modes are banned:

- **Never drop a label.** `relay:diag` means session `helm-diag` on `relay`. It does NOT collapse to plain `relay` (session `helm`). If they said `:diag`, I touch `helm-diag` — never the default pane.
- **Never substitute a sibling.** If they ask for `relay:ls` and `helm shell list relay` shows `helm-diag` already exists, I still use/create `helm-ls`. The presence of other labeled sessions is never a reason to reuse a different one — that is going behind the operator's back, the exact thing this skill exists to prevent.

Discipline:

1. **Parse literally.** `<alias>:<label>` → keep the label. `<alias>` alone → invent no label.
2. **Echo the resolved mapping before I act**, so any mismatch is visible before keystrokes land:
   > Opening `relay:ls` → tmux session `helm-ls` on relay.
3. **If the named session doesn't exist, create THAT one** (via `send`/`read` auto-create or `open -d <target>`) — never a sibling that happens to be lying around.
4. **If the target is ambiguous** (e.g. "the diag session" with no host, or no label given where context implies one), ask. Do not guess a host or a label.

The "default to one pane" guidance in *multi-pane workflows* below governs only the case where the operator gave **no** label. An explicit label always wins.

---

## the read-then-send discipline

**Never `send` blind.** Always `read` first, judge the shell state, then act.

```
helm shell read vps1 -n 50
```

Look for:

- **At a clean prompt** (e.g. `$ `, `# `, `user@host ~ % `) → safe to send.
- **Mid-command** (no trailing prompt; output still streaming or last line not a prompt) → either wait or abort what's running first. Do not pile new commands on top.
- **Interactive program** (vim, less, pager) → stop. Tell the operator the session is in vim/less and ask whether to send keys, wait, or have them switch out first.
- **Password prompt** (line ending in `:` containing `password` / `passphrase`) → stop. Tell the operator, do not type. They answer it in their attached tmux. **Only stop if the password line is the LAST non-empty line.** If a fresh shell prompt already appears below it, the prompt was satisfied (persistence, prior entry, or the program moved on) — proceed, do not pester the operator.
- **Failed command, undecided next step** → read more context, then check with operator before sending a fix.

Send one logical command at a time. Then `read` again to see the result before sending the next. Latency is a feature here — it's how the operator stays in the loop.

---

## verifying success

`send` returns instantly; it does not wait for the remote command to finish. After sending, sleep briefly (Bash tool's natural latency is usually enough) and `read` again to see the result. For long-running commands, poll `read` until you see the next prompt.

Example pattern:

```sh
helm shell read web -n 30      # check state
helm shell send web "doas rcctl restart httpd"
helm shell read web -n 10      # confirm exit / no errors
```

Do not assume success based on `helm shell send` exit code — that only confirms the keystrokes were delivered to tmux, not that the remote command succeeded.

---

## doas / sudo persistence: poll before asking

doas and sudo cache credentials **per-tty** for a window — and a tmux pane is one tty. The window is **5 minutes for doas, ~15 for sudo, and it slides**: every successful `doas`/`sudo` in that pane re-arms the timer to the full window (confirmed against OpenDoas and OpenBSD doas source — the timestamp is rewritten on each invocation, not just on password entry). So steady `doas` traffic in a pane keeps it unlocked indefinitely; a gap longer than the window with no `doas` lets it lapse.

**Per-pane scoping matters here.** The window lives on the pane's pty. When the operator types their password in their attached tmux, they arm the *same* pty I'm driving (same pane = same tty), which is exactly why persistence carries over to my sends. A different `:label` session is a different pane → a separate, independent window.

When the operator entered the password recently in that pane, the next `doas <cmd>` runs with **no prompt at all** — straight to the command's output. Telling them to "type the password" in that state is wrong: the command already ran, they have nothing to type, and they have to send `done` just to unblock me.

**The rule: never preemptively warn about a password. Send the doas/sudo command, then `read` and decide from actual scrollback.** Scrollback is ground truth; I cannot reliably clock the window between my own tool calls, and external activity can expire it without my seeing.

Two inputs sharpen the prior (but never replace the `read`):

- **The operator's word.** If they say "doas is good / I just entered it / it's persisted," treat the window as armed right now and proceed without a warning — then still `read` to confirm, since it may have lapsed in the gap since they spoke.
- **Time elapsed in-pane.** If many minutes have passed since the last `doas` here, *expect* the next one may re-prompt and handle it via the read flow — don't be surprised. If `doas` has been flowing steadily, expect persistence to hold. This shapes how I narrate, never a hard "it's fine, skip the read."

Three possible states after `helm shell send <target> "doas <cmd>"`:

1. **Persistence hit** — scrollback shows the command's output (or a fresh prompt with no output for a silent success). No `doas (user@host) password:` line anywhere after the send. → Treat as ran. Continue.
2. **Password prompt sitting open** — last non-empty line matches `doas \(.*\) password:` (or `[sudo] password for …:`) and there is no shell prompt below it. → Stop. Tell the operator to type it in their attached tmux. Poll `read` until the prompt clears.
3. **Mid-execution** — neither a password line nor a shell prompt yet (long-running command still streaming). → Poll `read` again. Do not assume password.

Poll cadence: `read` ~1s after `send`. If still ambiguous, `read` again 2-3s later. Do not announce a password prompt until you have actually seen one in scrollback. If persistence might have just expired, the command's first `read` will show the password line — handle it then, not preemptively.

---

## creating sessions

If `helm shell read` reports the session doesn't exist, `send` and `read` will create it automatically (the tmux session is auto-created on first use). That spawn-and-attach takes 1-2 seconds; the first `read` may show an empty pane or the login MOTD. Wait one more `read` cycle before judging.

To pre-create a session quietly: `helm shell open -d <target>`. Use this if you want the session warm before issuing commands.

---

## the `here` target: a pane in my own window

The operator runs one tmux session (one st terminal, windows per project) and I run inside a pane of it. `here[:label]` means: **a pane split inside the very window I'm running in.** No new terminal, no separate session. The pane lives in the operator's own tmux session, so it persists with that session — survives st crashes, and comes along when the operator re-attaches the session from another machine (e.g. the Mac).

This is the one target the helm CLI does not handle — I drive raw `tmux` against the local server instead. Every other discipline in this skill (read-then-send, target fidelity, doas persistence, secrets, narrate-before-send) applies unchanged; only the transport differs.

**Resolution is stateless, by pane tag, scoped to my current window.** The tag is a tmux pane *user option* (`@helm_label`) — it survives restarts, is invisible to the shell, and can't be clobbered by title-setting escape sequences. The pane title is set too, purely so the operator can identify the pane.

Canonical resolve-or-create, then send, then read:

```sh
LABEL=helm            # `here` → helm; `here:logs` → helm-logs
WIN=$(tmux display-message -p -t "$TMUX_PANE" '#{window_id}')
PANE=$(tmux list-panes -t "$WIN" -f "#{==:#{@helm_label},$LABEL}" -F '#{pane_id}' | head -1)
if [ -z "$PANE" ]; then
  PANE=$(tmux split-window -d -h -t "$TMUX_PANE" -P -F '#{pane_id}')
  tmux set-option -p -t "$PANE" @helm_label "$LABEL"
  tmux select-pane -t "$PANE" -T "$LABEL"
  # visible markers: border title on the pane, anchor flag on the window
  tmux set-option -w -t "$WIN" @helm_here 1
  tmux set-option -w -t "$WIN" pane-border-status top
  tmux set-option -w -t "$WIN" pane-border-format '#{?#{@helm_label}, #[fg=cyan]⚓ #{@helm_label}#[default] ,#{?#{@helm_viewport}, #[fg=yellow]👁 #{@helm_viewport}#[default] , #{pane_index}: #{pane_title} }}'
fi
tmux send-keys -t "$PANE" -l 'uptime' && tmux send-keys -t "$PANE" Enter
sleep 1
tmux capture-pane -t "$PANE" -p -S -200
```

(No `awk`/positional `$N` in these snippets, deliberately: when this skill loads via the `/helm-shell <args>` slash command, the command processor substitutes literal `$1`/`$2` in this file with the invocation args, corrupting any snippet that contains them. tmux's native `-f` filter does the matching instead.)

Rules:

- **Local shells only.** A here pane never holds a drivable ssh shell to another device — device work goes through `helm shell` (see *routing*). The in-window way to see a remote session is a *viewport pane* (next section), which I never type into.
- **Requires `$TMUX_PANE`.** If it's unset I'm not running inside the operator's tmux — say so and offer `local:<label>` instead. Never guess a window.
- **Always auto-split; never adopt an untagged pane.** An existing pane could be running vim, ssh, anything — sending keys into it is exactly the kind of behind-the-back action this skill bans. Only panes carrying `@helm_label` are mine to drive.
- **Labels mirror sessions:** bare `here` → tag `helm`; `here:logs` → tag `helm-logs`. Scoping is per-window, so each tmux window can have its own `helm` pane without collision. Target fidelity applies: `here:logs` never collapses to bare `here`.
- **Auto-create on use:** if the operator killed the pane, resolution finds nothing and the next send re-splits — same semantics as `helm shell send` auto-creating sessions.
- **No attach step exists or is needed.** The pane is immediately visible in the operator's window. The self-attach st flow below never applies to `here`.
- **Sizing:** default is a 50/50 vertical split to the **right** of my pane (`split-window -h`). If the operator asks for a below-split use `-v`; adjust size with `-l` on request.
- **Copy-mode is self-service here.** If sends stop landing (same scrollback on repeated reads), run `tmux send-keys -t "$PANE" -X cancel 2>/dev/null || true` to exit copy-mode myself — no need to ask the operator to press `q` like on remote sessions.
- **Visible marking:** a `here` pane must never look like "just a normal pane." Creation sets two markers (see snippet): per-window `pane-border-status top` whose format renders `⚓ <label>` on drivable panes and `👁 <target>` on viewport panes, and the window user option `@helm_here`, which the operator's `.tmux.conf` renders as a `⚓` after the window name in the status bar (the `#{?#{@helm_here}, ⚓,}` fragment in `window-status-format` / `window-status-current-format`). If that fragment is missing from the config, only the border title shows — say so rather than silently relying on it.
- **Close** with `tmux kill-pane -t "$PANE"` — same scrollback-purge benefit for leaked secrets as closing a session. A global `window-layout-changed` hook in the operator's `~/.tmux.conf` auto-drops the window markers when the last tagged pane (drivable or viewport) disappears (covers hand-closed panes too), so the manual cleanup below is belt-and-suspenders on hosts that have the hook. If that was the window's **last** tagged pane, drop the markers so the window reads normal again:

  ```sh
  tmux kill-pane -t "$PANE"
  if [ -z "$(tmux list-panes -t "$WIN" -f '#{||:#{!=:#{@helm_label},},#{!=:#{@helm_viewport},}}' -F x)" ]; then
    tmux set-option -w -t "$WIN" -u @helm_here
    tmux set-option -w -t "$WIN" -u pane-border-status
    tmux set-option -w -t "$WIN" -u pane-border-format
  fi
  ```
- **doas/sudo persistence** works per-pane as described above — the `here` pane is its own tty, and the operator can type a password directly into it since it's right in front of them.

---

## viewport panes: watching remote work in-window

When I start helm work on a remote device and `$TMUX_PANE` is set, I open a **viewport pane** by default — a pane in the operator's current window whose only job is showing the remote session live. The operator opts out per-request with "headless"; if `$TMUX_PANE` is unset there's nothing to split into, so I drive headless and tell them how to attach (`helm open <alias>`).

A viewport is not a here pane — I never type into it. It's spawned with the attach as its pane command, so there isn't even a shell underneath to receive keys:

```sh
ALIAS=relay          # exact target, label included: relay or relay:diag
WIN=$(tmux display-message -p -t "$TMUX_PANE" '#{window_id}')
VIEW=$(tmux list-panes -t "$WIN" -f "#{==:#{@helm_viewport},$ALIAS}" -F '#{pane_id}' | head -1)
if [ -z "$VIEW" ]; then
  VIEW=$(tmux split-window -d -h -t "$TMUX_PANE" -P -F '#{pane_id}' \
    "SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell open $ALIAS")
  tmux set-option -p -t "$VIEW" @helm_viewport "$ALIAS"
  tmux select-pane -t "$VIEW" -T "$ALIAS"
  tmux set-option -w -t "$WIN" @helm_here 1
  tmux set-option -w -t "$WIN" pane-border-status top
  tmux set-option -w -t "$WIN" pane-border-format '#{?#{@helm_label}, #[fg=cyan]⚓ #{@helm_label}#[default] ,#{?#{@helm_viewport}, #[fg=yellow]👁 #{@helm_viewport}#[default] , #{pane_index}: #{pane_title} }}'
fi
```

(`helm shell open` creates the session if missing, so spawning the viewport first also brings the session up — no separate `open -d` needed.)

Rules:

- **Tagged `@helm_viewport <target>`, never `@helm_label`** — resolution can never confuse a pane I drive with a pane that merely shows a remote. The border renders the difference: `⚓ helm-logs` (drivable) vs `👁 relay` (viewport).
- **One viewport per target per window — reuse, don't stack.** The resolve step finds an existing viewport before splitting. Target fidelity applies: `relay:diag` gets its own viewport, never reuses `relay`'s.
- **Driving still goes through `helm shell send <target>`**, reading through `helm shell read` — the CLI path over ssh, same as headless. The viewport only shows the keystrokes landing.
- **Password prompts land in front of the operator.** The viewport is a live attached client — they click in and type; doas persistence then carries to my sends (same remote pane, same tty).
- **Disposable.** The operator closing the pane just detaches a client; the session is untouched. Killing the session is still `helm shell close <target>` — the viewport pane exits with it (its pane command ends).
- **Cleanup** is the same either-tag check and `window-layout-changed` hook as here panes (see above) — hand-closed viewports drop the window markers too.

---

## the `local` escape hatch

`local[:label]` is a separate tmux session on the operator's machine — not a pane in their session. Normal local work routes to `here` panes; `local` exists for one case: **a shell that must outlive the current window** (park a build or a server, close the window, the session lives until reboot).

- Never routed to by default — only when the operator explicitly names `local` or asks for outlive-the-window persistence.
- Viewing it from inside tmux nests clients: a bare `helm shell open local` refuses while `$TMUX` is set. Either `tmux switch-client -t helm` (jumps the whole client; `switch-client -l` returns) or a viewport pane whose command is `TMUX= helm shell open local:<label>` (the `TMUX=` lets the nested client attach; prefix keys inside it need a double-tap).
- Everything else — read/send/list/close, doas persistence, secrets — behaves like any other helm session.

---

## self-attaching a terminal (opt-in only)

Remote targets normally don't need this — the auto-viewport already gives the operator eyes on the session. The flows below cover the leftovers: a target running headless, a `local` session, or operating outside tmux.

When the operator **explicitly tells me, in that message, to attach a terminal for them** — "attach a terminal", "open it for me", "pop up the session", "show me the session" — I may attach the named target so they can watch. It is never automatic and never a standing permission; each use needs a fresh explicit ask.

Mechanism, in order of preference:

- **Inside tmux (`$TMUX_PANE` set): a viewport pane** — exactly the *viewport panes* flow above (works for remote targets and, with `TMUX=`, the local escape hatch). A new tmux window instead of a pane on request: `tmux new-window -d -n <target> "SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell open <target>"`.
- **Outside tmux: an st window on the operator's X display** (requires `DISPLAY`; `:0` is the usual one), backgrounded so it doesn't block:

  ```sh
  # local target
  DISPLAY=:0 st -e helm shell open local:diag &

  # remote target — a local st window that ssh-attaches the remote session;
  # pass the agent socket so the child ssh can authenticate
  DISPLAY=:0 SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock st -e helm shell open relay:diag &
  ```

Rules:

- **Target fidelity still holds** — attach exactly the named target, then confirm: "Opened a viewport for `relay:diag`."
- **The st path needs a local display.** If `DISPLAY` is unset (e.g. headless session) and there's no tmux either, say so and fall back to asking the operator to run `helm shell open <target>` themselves. Don't silently no-op.

---

## multi-pane workflows

For a host where the operator wants me working in one pane while they work in another (or while a long-running command stays in its own pane), use labels:

- `vps1` = the default shared shell
- `vps1:deploy` = a separate pane for deploys
- `vps1:logs` = a pane I park a `tail -f` in so I can read it without blocking the main shell

Each `:label` creates a separate remote tmux session, each with its own viewport pane (one viewport per target — see *viewport panes*).

**Default to ONE pane per host (the default `<alias>` target). Only spawn a labeled pane when the operator explicitly asks for one.** Multiple labeled panes fragment context, are easy to leave stuck in tmux copy-mode (where keystrokes get silently swallowed), and force the operator to attach to the right one to see live output. If a previous session has left labeled panes around, close them with `helm shell close <target>` at the next opportunity.

This default applies **only when the operator gave no label**. When they name `<alias>:<label>` explicitly, I open exactly that — see *target fidelity* above. Defaulting to one pane never means collapsing a label they did provide.

---

## panes can be in copy-mode (silent failure)

If `helm shell send` appears to deliver but the command never executes — no new prompt, no echoed output, repeated `read` returns the same scrollback — the pane is most likely in tmux copy-mode (operator scrolled up with a mouse wheel or paged through history). Sends still land at the bottom of the buffer, but the visible window is frozen at the scrolled-up position, so `helm shell read` captures stale content. The shell underneath is fine; only the view is wedged.

Tells:
- Sends followed by reads keep showing the same lines.
- `helm shell read` output ends with `(N lines truncated)` even when you asked for plenty.
- No prompt at the bottom of the visible region.

Fix:
1. Ask the operator to press `q` (or `Esc`) in their attached terminal to leave copy-mode.
2. If they're away, close + reopen the pane: `helm shell close <target>` then `helm shell open -d <target>`. Loses scrollback (often a feature — purges any secrets that leaked into it).

---

## visibility hand-off

The operator's primary view is the pane in their window — the viewport for remote sessions, the here pane for local ones. They see every keystroke I send the moment it lands. They don't need to look at my conversation to track what I did; they look at their pane.

But: I should still narrate intent in conversation before sending. Two sentences max. "About to restart httpd on web. Reading state first." Then execute. This gives the operator time to interrupt if my intent is wrong, even before the keys land in their tmux.

After a multi-step shell session, summarize what happened in one sentence — what was done, what's still open. The operator may have stepped away; the summary lets them catch up without reading the whole tmux pane.

---

## when NOT to use this skill

- The operator wants a quick one-shot output to read in our conversation → use `helm exec <alias> <cmd>` instead.
- The host has no `helm shell` session open AND the operator hasn't asked me to create one → don't create speculatively; ask first.
- A password prompt **actually appears in scrollback** (sudo/doas with expired persistence, ssh-key passphrase, etc.) → stop and tell the operator. Do not preempt — see the doas/sudo persistence section above.
- Destructive operations (`rm -rf`, `doas pkg_delete -X`, dropping a database, force-pushing) → narrate intent and wait for explicit go-ahead before sending.

---

## secrets in scrollback (read pulls them into context)

`helm shell read` captures the visible pane verbatim. Anything printed in that window — env-file `grep` output, third-party API responses, command echo — lands in my context, and from there in the conversation transcript. Treat every read as a potential leak.

**Discipline:**

1. **Never print a secret directly.** If I need to know whether `FOO_TOKEN` is set, count or boolean-check: `doas grep -c '^FOO_TOKEN=' /etc/<svc>/env` returns `0` or `1`, not the value. Avoid `grep '^FOO_TOKEN=' …` which prints the line.
2. **Never inline a secret in `helm shell send "<text>"`.** The keystrokes are visible in the pane (and to the operator, fine, but also to my next `read`). If the operator must enter a value, have them type it directly in their attached terminal.
3. **Filter every third-party API response.** Many APIs return webhook auth passwords, OAuth client secrets, and similar fields in `GET` payloads. Before reading the pane, sanitize via a single sed pass that covers the common field names:

   ```sh
   sed -E 's/"(Password|Token|Secret|ApiToken|ServerToken|AccountToken|ClientSecret|PrivateKey|BasicAuthPassword|HttpAuthPassword)":"[^"]*"/"\1":"<REDACTED>"/g'
   ```

   Pipe the API response through this BEFORE the bytes hit the pane (e.g. `curl … | sed -E '…' | tee /tmp/resp.json`). Filtering after the fact does not help — the unredacted bytes already passed through the pane and into scrollback.

4. **If a secret leaks anyway, alert the operator and offer to rotate.** Do not silently move on. The operator decides whether to rotate now or batch at session end (long iterative work can defer; a finished feature should rotate before being declared done).

5. **Closing + reopening a pane purges its scrollback.** When several secrets have accumulated in a pane during debugging, `helm shell close <target>` then re-open is a cheap way to drop them from any future `read`.

Pairs with any per-project secrets-handling notes.

---

## ssh-agent bridge (required for remote targets only)

`local` and `here` targets skip ssh entirely, so no agent setup is needed for them.

For ssh aliases, my Bash invocations don't inherit `SSH_AUTH_SOCK` from the operator's interactive terminals — different process tree, no shared env. Viewport pane commands don't either, which is why the spawn snippet embeds the socket inline. The operator runs a persistent agent at a fixed path so I can reach it:

```sh
# operator (once per boot)
ssh-agent -a /tmp/<user>-ssh-agent.sock
export SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock
ssh-add ~/.ssh/<vps-key>
```

I must prefix every remote `helm shell` (and bare `ssh`) call with that socket — the harness resets env between Bash calls, so exporting in one call does not stick:

```sh
SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell read vps1 -n 50
```

If a call fails with `Permission denied (publickey)` or `Could not open a connection to your authentication agent`: ask the operator to verify `ls /tmp/<user>-ssh-agent.sock` exists and `SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock ssh-add -l` lists the VPS key. If empty, they re-run the spawn sequence above.

---

## reference: helm shell CLI

For completeness; refer to `helm shell help` on the operator's machine for the live source of truth.

| command | purpose |
|---|---|
| `helm shell open <target>` | operator-attaches a terminal to the session (I run it only as a viewport pane's command, or via the opt-in *self-attaching a terminal* flow) |
| `helm shell open -d <target>` | pre-create the session detached |
| `helm shell send <target> "<text>"` | type a line into the active pane (auto-Enter) |
| `helm shell read <target> [-n LINES]` | capture the active pane's scrollback |
| `helm shell list <alias>` | list helm-* sessions on the alias's tmux server (use `local` for the operator's machine) |
| `helm shell close <target>` | kill the session |

`<target>` examples: `vps1` (default session on VPS `vps1`), `vps1:deploy` (labeled session on that VPS), `local` (escape-hatch session on the operator's own machine — see *the `local` escape hatch*), `local:agent` (labeled session there). The session name on the host is `helm` (no label) or `helm-<label>` (with label). All my interactions go through `helm shell`; I never `tmux` directly — with two exceptions: `here[:label]` panes and viewport panes live in my own tmux window, which the helm CLI has no concept of, so those are managed via raw `tmux` as described in their sections (viewports are spawned by raw tmux but never typed into).
