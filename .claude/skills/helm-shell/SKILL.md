---
name: helm-shell
description: "Drive a persistent shell the operator and I both watch live — a helm tmux session on a remote ssh alias (web, vps1, relay, …), or a pane in the operator's current tmux window. Local shell work defaults to an in-window pane (the operator lives in tmux); 'local' is the escape hatch for a session that must outlive the window. Remote work auto-opens a read-only viewport pane so the operator sees every keystroke (opt out with 'headless'); sessions survive disconnects. Everything routes through the helm CLI: remote sessions via 'helm shell' (open/run/send/key/read/list/close), in-window panes via 'helm pane' (open/view/send/key/read/close/list) — I never drive raw tmux. For a non-interactive command whose result I just need, prefer 'helm shell run <target> <cmd>' (or 'helm pane run' in a pane): one call returns the command's output plus its exit code, no read-then-send loop. 'helm shell key' / 'helm pane key' send raw key specs (Up, C-c, Escape) to drive a full-screen TUI — including on a remote host over ssh. Read-then-send keeps the operator in the loop for interactive or risky work — they can intervene any moment (type a password, Ctrl-C) because it's their tmux. CONSIDER IT PROACTIVELY, not only on explicit request: together with one-shot 'helm exec' it is one of the only two ways I can run anything on another machine, so route remote work here rather than assuming I cannot help. Reach for it automatically when — the task runs commands on a remote device/server/VPS and is interactive, multi-step, or stateful (deploy, restart a service, tail logs, run a migration, debug on the box); a command needs interactive doas/sudo or an ssh passphrase the operator must type; the operator should watch and be able to interrupt; shell state matters (cwd/env/history persist) or a helm shell is already open; or something long-lived/visible should sit in a pane — a dev server, build, or 'tail -f'. Explicit triggers still apply: 'send to helm', 'run in my helm shell', 'run it here', 'open/split a pane here', 'watch <host> in a pane', 'drive the TUI'. SKIP a quick one-shot stateless command whose output I just need — that's 'helm exec <alias> <cmd>' or direct ssh, simpler."
---

# helm-shell

A bridge skill. The operator runs persistent shells — a tmux session on a remote host, or a pane in their own tmux window. They watch live by attaching; I drive from the side: read scrollback, type commands, never blind. The operator can intervene at any moment — passwords, Ctrl-C, anything — because it is their tmux.

Two surfaces, both driven through the **helm CLI** (never raw tmux):

- **`helm shell`** — a tmux session on a chosen host. A real ssh alias (`web`, `vps1`) → the session runs on that device over ssh; this is the ONLY way I drive another machine. By default the session gets a read-only **viewport pane** in the operator's window (`helm pane view`) so they watch live.
- **`helm pane`** — a pane inside the very tmux window I'm running in, on the operator's own machine. This is the default for local shell work: the operator lives in tmux, so "open a pane / a local shell / run it here" just splits a pane here. No ssh.

The reserved alias **`local`** is a separate escape hatch — a tmux *session* (not a pane) on the operator's machine, for a shell that must outlive the current window. Not part of normal routing; see *the `local` escape hatch*.

This is fundamentally different from `helm exec`:

- `helm exec` = one-shot, output streams back to my conversation, no shell state retained.
- `helm shell` / `helm pane` = stateful long-lived shell, output renders in the operator's tmux (and I scrape it), shell state (cwd, env, history, in-progress prompts) persists.

Use a shell/pane when the operator wants to *watch* me work or when shell state matters. Use `helm exec` otherwise.

Two sibling surfaces need no shell at all:

- **Read verbs** — a quick read of fleet state: `helm ls`, `show <host>`, `svc <host>`, `ps <host>`, `ports <host>`, `vultr`, `logs <host>`, `history [<id>]` (bare lists recent `helm exec` runs; with an id, prints that run's full transcript), `activity` (add `--json` for machine output). Lighter than opening a shell just to eyeball state.
- **Mutating verbs** are operator-only: `helm vultr {reboot,halt,start,snapshot} <id>` and `helm run <key> <host>` refuse without `--yes` and sit off my un-gated surface. I never invoke them — if a mutation is wanted, I narrate intent and let the operator run it.

---

## routing: which surface a request means

The differentiator is **where the shell process runs**, never where it's viewed:

| operator says | shell runs | surface | operator watches via |
|---|---|---|---|
| names a device (`web`, `vps1`, `relay`, …) | that device | `helm shell` over ssh | auto-opened viewport pane here |
| names a device + "headless" | that device | `helm shell` over ssh | attaches when they want (`helm open <alias>`) |
| "a pane", "a local shell", "run it here", or no device named | this machine | `helm pane` in my window | the pane itself |
| explicitly `local`, or "outlive the window" | this machine, separate session | `helm shell` to `local`, no ssh | see *the `local` escape hatch* |

- **No device named → a pane here.** Local shell work defaults to `helm pane` — a pane in my window. There is no separate "here" keyword to reach for; the pane *is* the default. (If the operator says the word "here" it still means exactly this: a pane in this window.)
- **A pane never holds a drivable ssh shell to another device.** Device work goes through `helm shell` over ssh. The in-window way to *see* a remote session is a viewport (`helm pane view`), which I never type into.
- **One surface per shell.** Anything with a host goes through `helm shell`; in-window panes go through `helm pane`. A viewport showing a remote session is read-only to me — driving it is banned; I drive the remote via `helm shell send/key`, the viewport just shows it.

---

## the CLI surface

All commands run via the Bash tool. Remote `helm shell` calls need the ssh-agent socket prefix (see *ssh-agent bridge*); `helm pane` and `local` need no ssh.

**Remote sessions — `helm shell`:**

```sh
helm shell list <alias>                 # which helm-* sessions exist on the host (alias or `local`)
helm shell open -d <target>             # create the session detached, no attach
helm shell run  <target> "<cmd>"        # run one command, get back its output + `exit: N` (one call)
helm shell send <target> "<text>"       # type a line (auto-Enter)
helm shell key  <target> <key...>       # send raw key specs (Up, C-c, Escape) — drive a TUI
helm shell read <target> [-n LINES]     # scrape scrollback (default 200; trailing blanks stripped, --raw keeps them)
helm shell close <target>               # kill the session
```

**In-window panes — `helm pane`** (requires `$TMUX_PANE`; I'm running in the operator's tmux):

```sh
helm pane open  [-l LABEL] [--below] [--size N]   # resolve-or-create a drivable pane
helm pane view  <target> [--below] [--size N]     # read-only viewport onto a remote helm session
helm pane send  [-l LABEL] "<text>"               # type a line (auto-Enter)
helm pane run   [-l LABEL] "<cmd>"                # run one command, get back its output + exit: N
helm pane key   [-l LABEL] <key...>               # raw key specs (no Enter) — drive a local TUI
helm pane read  [-l LABEL] [-n N] [--raw]         # capture (default 200, trailing blanks stripped)
helm pane close [-l LABEL]                         # kill the drivable pane
helm pane list                                     # list helm panes in this window
```

`<target>` for `helm shell` is `<alias>` (default session `helm`) or `<alias>:<label>` (session `helm-<label>`). For `helm pane`, the default pane is bare (`-l` omitted → tag `helm`); `-l logs` is a second pane (tag `helm-logs`). The attaching `helm shell open <target>` (no `-d`) is operator-facing — I run it only as a viewport's pane command (which `helm pane view` does for me) or in the opt-in self-attach flow. I never attach a terminal for myself.

---

## prefer `run` for a command whose result I just need

For a **non-interactive command on a remote session**, reach for `helm shell run` first:

```sh
SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell run web "doas rcctl restart httpd"
# → (the command's own output)
# exit: 0
```

One call sends the command, waits for it to finish (a single ssh round-trip — the wait happens on the host), and returns **only that command's output plus `exit: N`**. No read-then-send loop, no eyeballing "did it work" — the exit code is the answer. `run` is single-line, non-interactive only:

- A pane sitting in a pager/editor, or mid-command, is reported `busy` → fall back to `read`/`send`, or `key` for a TUI.
- A command that exits the shell (`exit`, `logout`) is reported as the session being gone.
- Newlines are rejected (they'd break the exit-code capture). One command per call.

For **in-window panes**, `helm pane run [-l LABEL] "<cmd>"` is the same one-shot — output + `exit: N` in a single call. It's local (no ssh round-trip to collapse), but the exit code and the definite "done" signal still beat eyeballing `read`.

`run` does NOT replace read-then-send for **interactive or risky** work (see next). It's the fast path for "restart the service / check disk / run the migration step and tell me the exit code."

---

## the read-then-send discipline (interactive / uncertain state)

When I'm *not* using `run` — an interactive program, an uncertain pane state, a risky mutation the operator should watch land — **never `send` blind. `read` first, judge the state, then act.**

```sh
SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell read vps1 -n 50
```

Look for:

- **At a clean prompt** (`$ `, `# `, `<user>@host ~ % `) → safe to send.
- **Mid-command** (no trailing prompt; output still streaming) → wait or abort first. Don't pile on.
- **Interactive program** (vim, less, pager) → stop, or drive it deliberately with `key` (see *driving TUIs*). Don't fire a shell line into it by accident.
- **Password prompt** (line ending in `:` containing `password`/`passphrase`) → stop, tell the operator, do not type. They answer in their attached tmux. **Only stop if the password line is the LAST non-empty line.** If a fresh prompt already appears below it, the prompt was satisfied — proceed, don't pester.
- **Failed command, undecided next step** → read more context, check with the operator before sending a fix.

Send one logical command at a time, then `read` again before the next. Latency is a feature here — it's how the operator stays in the loop.

---

## verifying success (send/read path)

`send` returns instantly; it does not wait for the remote command to finish. After sending, sleep briefly (the Bash tool's natural latency is usually enough) and `read` again. For long-running commands, poll `read` until the next prompt.

```sh
SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell read web -n 30   # check state
SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell send web "doas rcctl restart httpd"
SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell read web -n 10   # confirm
```

`send`'s exit code only confirms the keystrokes reached tmux, never that the remote command succeeded. (`run` is the exception — its `exit: N` *is* the remote command's status.)

---

## driving TUIs with `key`

`send` types a line and presses Enter — it can't drive a full-screen program. `key` sends raw tmux key specs with **no Enter appended**, so I can operate vim, htop, a menu, a pager — and, crucially, **on a remote host over ssh** (which nothing else here can do):

```sh
SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell key vps1 Down Down Enter   # menu navigation
SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell key vps1 C-c               # interrupt
SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell key web Escape : w q Enter  # save+quit vim
helm pane key C-c                                                             # local pane
```

Key specs are tmux's: `Up Down Left Right`, `Enter`, `Escape`, `Tab`, `BSpace`, `C-c` (Ctrl-C), `M-x` (Alt-x), `F1`…`F12`, `Space`, `PageUp`/`PageDown`. Multiple keys in one call are sent in order. To send literal text (not key names), use `send` (which presses Enter) — there is no literal-without-Enter mode, so a literal word like "Enter" can't be typed via `key`. `read` after driving to see the screen. The operator watches every keystroke land in their viewport and can take over instantly.

---

## doas / sudo persistence: poll before asking

doas and sudo cache credentials **per-tty** — and a tmux pane is one tty. The window is **5 minutes for doas, ~15 for sudo, and it slides**: every successful `doas`/`sudo` in that pane re-arms the timer to the full window (confirmed against OpenDoas and OpenBSD doas source — the timestamp is rewritten on each invocation). So steady `doas` traffic keeps a pane unlocked indefinitely; a gap longer than the window lets it lapse.

**Per-pane scoping matters.** When the operator types their password in their attached tmux, they arm the *same* pty I'm driving (same pane = same tty), which is why persistence carries to my sends. A different `:label` session is a different pane → a separate window. The same holds for `run`, `send`, and `key` — they all land in that one pane's tty.

When the operator entered the password recently, the next `doas <cmd>` runs with **no prompt at all** — straight to output. Telling them to "type the password" then is wrong: the command already ran.

**The rule: never preemptively warn about a password. Run the doas/sudo command, then judge from actual scrollback.** With `run`, the `exit: N` and output tell the story; with `send`, `read` and decide. Scrollback is ground truth — I can't reliably clock the window between my own tool calls, and external activity can expire it.

Two inputs sharpen the prior (but never replace the read):

- **The operator's word.** "doas is good / I just entered it" → treat as armed and proceed — then still confirm, since it may have lapsed in the gap.
- **Time elapsed in-pane.** Many minutes since the last `doas` → *expect* a possible re-prompt and handle it via the read flow. Steady `doas` traffic → expect persistence to hold. This shapes how I narrate, never a hard "skip the read."

After a `doas` `send`, three states (read to disambiguate): **persistence hit** (output or fresh prompt, no `password:` line) → ran, continue; **prompt sitting open** (last non-empty line matches `doas \(.*\) password:` / `[sudo] password for …:`, no prompt below) → stop, tell the operator to type it in their tmux, poll until it clears; **mid-execution** (neither, long command streaming) → poll again, don't assume password. Poll ~1s after send, again 2–3s later if ambiguous. Never announce a password prompt until I've actually seen one.

---

## creating sessions and panes

- **`helm shell`**: `send`/`run`/`read` auto-create the session on first use (1–2s; the first `read` may show an empty pane or the login MOTD — wait one more cycle). To pre-warm quietly: `helm shell open -d <target>`.
- **`helm pane`**: `open`/`send`/`key`/`read` auto-create the pane on first use (splits my window, tags it, marks the window). `helm pane open` pre-creates it without sending anything.

---

## helm pane: panes in my own tmux window

When the operator wants a local shell, or wants to watch a remote session in-window, the pane lives in **the very window I'm running in**, on their own tmux. `helm pane` manages it for me — splitting, tagging, the visible border markers, and cleanup — so I never touch raw tmux. The pane persists with the operator's tmux session: it survives terminal crashes and comes along when they re-attach from another machine.

Two kinds:

- **Drivable pane** (`helm pane open/send/key/read/close`) — a local shell I type into. The default is the bare pane (`-l` omitted); `-l <label>` is a second pane (e.g. `-l logs` for a parked `tail -f` I read without blocking the main pane). Each label is its own pane, scoped to this window.
- **Viewport pane** (`helm pane view <target>`) — a read-only client attached to a remote `helm shell` session, so the operator watches the remote work live. I never type into it; I drive the remote through `helm shell send/run/key` and the viewport shows it. This is what auto-opens for remote work (opt out: "headless"). One viewport per target — `view` reuses an existing one.

Rules:

- **Requires `$TMUX_PANE`.** If it's unset I'm not inside the operator's tmux — `helm pane` says so and I offer `local:<label>` or a headless remote session instead. I never guess a window.
- **Auto-split, never adopt a stray pane.** `helm pane` only ever drives panes it tagged (`@helm_label`); an untagged pane could be running anything. Driving it is the kind of behind-the-back action this skill bans.
- **Target fidelity.** `-l logs` is the `helm-logs` pane and only that — it never collapses to the bare pane, and the presence of other labeled panes is never a reason to reuse one.
- **Default to ONE pane.** Spawn a labeled pane only when the operator asks; multiple panes fragment context and are easy to leave wedged in copy-mode. Close leftover labeled panes with `helm pane close -l <label>` at the next opportunity.
- **Close** with `helm pane close [-l LABEL]` — kills the pane and (when no helm pane remains) drops the window markers; the operator's `~/.tmux.conf` hook also handles panes they close by hand. Closing + reopening purges scrollback — a cheap way to drop leaked secrets.
- **Sizing/placement:** default is a 50/50 split to the right; `--below` for a horizontal split, `--size N` for a specific size.
- **Copy-mode is self-service here** — if sends stop landing (same scrollback on repeated reads), the pane is in copy-mode; closing+reopening clears it (see *copy-mode* below).
- **doas/sudo persistence** works per-pane — the pane is its own tty, and the operator can type a password right into it since it's in front of them.

---

## the `local` escape hatch

`local[:label]` is a separate tmux *session* on the operator's machine — not a pane in their window. Normal local work routes to `helm pane`; `local` exists for one case: **a shell that must outlive the current window** (park a build or a server, close the window, the session lives until reboot). Driven via `helm shell` (the `local` alias skips ssh).

- Never routed to by default — only when the operator names `local` or asks for outlive-the-window persistence.
- Viewing it from inside tmux nests clients: a bare `helm shell open local` refuses while `$TMUX` is set. Either `tmux switch-client -t helm` (jumps the whole client; `switch-client -l` returns), or a viewport via `helm pane view local` is not available (viewports are for ssh targets) — instead `TMUX= helm shell open local:<label>` in a new window/pane (the `TMUX=` lets the nested client attach; prefix keys need a double-tap).
- Everything else — read/run/send/key/list/close, doas persistence, secrets — behaves like any other helm session.

---

## self-attaching a terminal (opt-in only)

Remote targets normally don't need this — the auto-viewport already gives the operator eyes. The flows below cover the leftovers: a headless target, a `local` session, or operating outside tmux.

When the operator **explicitly tells me, in that message, to attach a terminal for them** — "attach a terminal", "open it for me", "pop up the session", "show me the session" — I may. Never automatic, never a standing permission; each use needs a fresh ask.

In order of preference:

- **Inside tmux (`$TMUX_PANE` set):** `helm pane view <target>` (remote) — a viewport pane. A new window instead, on request: `tmux new-window -d -n <target> "SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell open <target>"`.
- **Outside tmux: an st window on the operator's X display** (needs `DISPLAY`; `:0` is usual), backgrounded:

  ```sh
  DISPLAY=:0 st -e helm shell open local:diag &
  DISPLAY=:0 SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock st -e helm shell open relay:diag &
  ```

Target fidelity still holds — attach exactly the named target, then confirm. If `DISPLAY` is unset and there's no tmux either, say so and fall back to asking the operator to run `helm shell open <target>` themselves. Don't silently no-op.

---

## panes can be in copy-mode (silent failure)

If a `send`/`key` appears to deliver but nothing executes — no new prompt, repeated `read` returns the same scrollback — the pane is most likely in tmux copy-mode (the operator scrolled up with the mouse). Sends still land at the bottom, but the captured view is frozen at the scrolled position, so `read` shows stale content. The shell underneath is fine; only the view is wedged.

Tells: repeated reads show the same lines; `read` ends with `(N lines truncated)` even with plenty asked; no prompt at the bottom of the visible region.

Fix: ask the operator to press `q`/`Esc` in their terminal; or, if they're away, close + reopen the pane (`helm shell close`/`open -d`, or `helm pane close`/`open`) — loses scrollback, often a feature (purges leaked secrets).

---

## visibility hand-off

The operator's primary view is their pane — the viewport for remote sessions, the drivable pane for local. They see every keystroke the moment it lands; they don't need to read my conversation to track what I did.

Still: **narrate intent before sending. Two sentences max.** "About to restart httpd on web. Reading state first." Then act. This gives the operator time to interrupt if my intent is wrong, before the keys land. After a multi-step session, summarize in one sentence — what was done, what's still open — so they can catch up without reading the whole pane.

---

## when NOT to use this skill

- The operator wants a quick one-shot output to read in our conversation → `helm exec <alias> <cmd>`.
- No session/pane is open AND the operator hasn't asked for one → don't create speculatively; ask first.
- A password prompt **actually appears in scrollback** (expired persistence, ssh-key passphrase) → stop and tell the operator. Don't preempt — see *doas/sudo persistence*.
- Destructive operations (`rm -rf`, `doas pkg_delete -X`, dropping a database, force-pushing) → narrate intent and wait for explicit go-ahead before sending. `run` does not bypass this — a destructive command is operator-confirmed first.

---

## secrets in scrollback (read pulls them into context)

`read` (and `run`'s captured output) pulls the visible pane verbatim into my context, and from there into the transcript. Treat every read as a potential leak.

1. **Never print a secret directly.** To check whether `FOO_TOKEN` is set, count or boolean-check: `doas grep -c '^FOO_TOKEN=' /etc/<svc>/env` returns `0`/`1`, not the value. Avoid `grep '^FOO_TOKEN=' …`.
2. **Never inline a secret in `send`/`run`.** The keystrokes are visible in the pane and to my next read. If the operator must enter a value, they type it directly in their attached terminal.
3. **Filter every third-party API response** before the bytes hit the pane. Many APIs return secrets in `GET` payloads. Sanitize in the same pipeline:

   ```sh
   curl … | sed -E 's/"(Password|Token|Secret|ApiToken|ServerToken|AccountToken|ClientSecret|PrivateKey|BasicAuthPassword|HttpAuthPassword)":"[^"]*"/"\1":"<REDACTED>"/g' | tee /tmp/resp.json
   ```

   Filtering after the fact doesn't help — the unredacted bytes already passed through the pane.
4. **If a secret leaks anyway, alert the operator and offer to rotate.** Don't silently move on. They decide rotate-now vs batch-at-session-end (long iterative work can defer; a finished feature rotates before it's declared done).
5. **Closing + reopening a pane/session purges its scrollback** — a cheap drop for accumulated secrets.

Pairs with the per-project `feedback_helm_shell_pane_leaks_secrets.md` memory if one exists.

---

## ssh-agent bridge (remote targets only)

`local` and `helm pane` skip ssh entirely — no agent setup. For ssh aliases, my Bash invocations don't inherit `SSH_AUTH_SOCK` from the operator's interactive terminals (different process tree). The operator runs a persistent agent at a fixed path:

```sh
# operator (once per boot)
ssh-agent -a /tmp/<user>-ssh-agent.sock
export SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock
ssh-add ~/.ssh/<vps-key>
```

I prefix every remote `helm shell` call with that socket (the harness resets env between Bash calls, so exporting in one call doesn't stick):

```sh
SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell read vps1 -n 50
```

`helm pane view` embeds the socket into the viewport's pane command automatically (it reads my current `SSH_AUTH_SOCK`), so the viewport's child ssh authenticates too. If a call fails with `Permission denied (publickey)` or `Could not open a connection to your authentication agent`: ask the operator to verify `ls /tmp/<user>-ssh-agent.sock` exists and `SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock ssh-add -l` lists the VPS key; if empty, re-run the spawn sequence above.

---

## reference: the CLI

Refer to `helm shell help` and `helm pane help` on the operator's machine for the live source of truth.

| `helm shell` | purpose |
|---|---|
| `open <target>` | operator-attaches a terminal (I run it only as a viewport command or the opt-in self-attach) |
| `open -d <target>` | pre-create the session detached |
| `run <target> "<cmd>" [--timeout S]` | run one non-interactive command; print its output + `exit: N` (single ssh round-trip) |
| `send <target> "<text>"` | type a line (auto-Enter) into the active pane |
| `key <target> <key...>` | send raw tmux key specs (no Enter) — drive a TUI |
| `read <target> [-n LINES] [--raw]` | capture scrollback (default 200; trailing blanks stripped unless `--raw`) |
| `list <alias>` | list helm-* sessions on the alias's server (`local` for the operator's machine) |
| `close <target>` | kill the session |

| `helm pane` (needs `$TMUX_PANE`) | purpose |
|---|---|
| `open [-l LABEL] [--below] [--size N]` | resolve-or-create a drivable pane in my window |
| `view <target> [--below] [--size N]` | resolve-or-create a read-only viewport onto a remote session |
| `send [-l LABEL] "<text>"` | type a line (auto-Enter) |
| `run [-l LABEL] "<cmd>" [--timeout S]` | run one non-interactive command; print its output + `exit: N` |
| `key [-l LABEL] <key...>` | send raw key specs (no Enter) |
| `read [-l LABEL] [-n N] [--raw]` | capture the pane (default 200, trailing blanks stripped) |
| `close [-l LABEL]` | kill the drivable pane (markers torn down when none remain) |
| `list` | list helm panes in this window |

`<target>` examples: `vps1` (default session on VPS `vps1`), `vps1:deploy` (labeled session), `local` / `local:claude` (escape-hatch session on the operator's machine). Session name on the host: `helm` (no label) or `helm-<label>`. Everything goes through the helm CLI — I never run `tmux` directly.
