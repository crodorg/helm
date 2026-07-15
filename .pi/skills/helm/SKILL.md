---
name: helm
description: "Drive a persistent shell the operator and I both watch live — a tmux session on a remote ssh alias (web, vps1) or a pane in the operator's current tmux window — via the helm CLI, never raw tmux. Route machine operations here PROACTIVELY (remote or this box): deploy, restart a service, migrations, log tails, long-lived panes, anything needing doas/sudo or an ssh passphrase. Quick state checks: helm svc/health/ports/show. Triggers: 'send to helm', 'run it here', 'open a pane here'. SKIP a stateless one-shot whose output I just need — 'helm exec <alias> <cmd>' or ssh."
---

# helm

A bridge skill. The operator runs persistent shells in tmux; I drive from the side — read scrollback, type commands, never blind. They watch live and can intervene (passwords, Ctrl-C) any moment because it's their tmux.

Two surfaces, both driven through the **helm CLI** (never raw tmux), differentiated by **where the shell runs**:

- **`helm shell`** — a tmux session on a remote host over ssh. The ONLY way I drive another machine. Auto-opens a **viewport pane** (`helm pane view`) in the operator's window so they watch live; the viewport is read-only *to me* — I drive the remote via `helm shell send/run/key`, the viewport just shows it. The operator *can* type into the viewport (it's a live attach for them): that's where they enter a doas password or hit Ctrl-C, and since it's the same remote tty I drive, a password they type arms doas for my next command.
- **`helm pane`** — a pane in the very tmux window I'm running in, on the operator's own machine. Default for local shell work ("open a pane", "run it here", no device named). No ssh.

Fundamentally different from `helm exec <alias> <cmd>` — one-shot, output streams back to my conversation, no shell state. Use a shell/pane when the operator wants to *watch* or when shell state (cwd, env, history) matters; `helm exec` otherwise.

**Read verbs** — `helm ls/show/svc/ps/ports/vultr/logs/history[<id>]/activity` (`--json` for machine output) — are quick fleet state, lighter than opening a shell. **Mutating verbs** (`vultr reboot|halt|start|snapshot`, `helm run`) are operator-only (refuse without `--yes`); I never invoke them — I narrate intent and let the operator run them.

---

## the default: open the surface first, then keep going

When the operator points me at a machine — names a host (`web`, "get me into web", "watch web") or asks for a local shell — **opening the surface is my first action, every time.** Remote → `helm pane view <target>` *before* I run anything (so they watch from keystroke one), then drive via `helm shell`. Local → `helm pane open`/`send` splits the pane on first use. Don't run, then offer to open a pane — that two-step handoff is what this skill kills.

Then **keep going** — drive to conclusion, pausing only for: a real password prompt, a destructive/risky mutation, or genuinely uncertain pane state. Don't stop to ask "continue?" between routine steps.

**Opt out ("headless" / "in the background"):** skip the viewport, run quietly, report back — only when the operator says so. (About *my* visibility, not backgrounding a process — "run the build in the background" is a parked job, not headless. When ambiguous, the viewport is cheap: open it.)

---

## the CLI surface

All commands run via the Bash tool. Remote `helm shell` calls need the ssh-agent socket prefix (see *ssh-agent bridge*); `helm pane` and the `local` alias need none.

**One verb set, two targets.** `helm shell <verb> <target>` drives a remote session over ssh; `helm pane <verb> [-l LABEL]` drives a local pane in my window. Verbs match across both:

| verb | shell: `helm shell <v> <target>` | pane: `helm pane <v> [-l LABEL]` |
|---|---|---|
| `open` | `open -d <target>` pre-create detached; `open <target>` = operator-attach (I run it only as a viewport's pane cmd) | `open [-l LABEL] [--below] [--size N]` resolve-or-create a drivable pane |
| `view` | — | `view <target> [--below] [--size N]` read-only viewport onto a remote session |
| `run` | `run <target> "<cmd>" [--timeout S]` one-shot; output + `exit: N` | `run [-l LABEL] "<cmd>" [--timeout S]` same |
| `send` | `send <target> "<text>"` type a line (auto-Enter) | `send [-l LABEL] "<text>"` |
| `key` | `key <target> <key...>` raw tmux keys (no Enter) — drive a TUI | `key [-l LABEL] <key...>` |
| `read` | `read <target> [-n L] [--raw\|--delta]` capture (default 200) | `read [-l LABEL] [-n N] [--raw\|--delta]` |
| `wait` | `wait <target> [--timeout S]` block until back at a prompt | `wait [-l LABEL] [--timeout S]` (resolve-only) |
| `watch` | `watch <target> [--idle\|--match REGEX] [--timeout S]` | `watch [-l LABEL] [--idle\|--match REGEX] [--timeout S]` |
| `list` | `list <alias>` helm-* sessions on the host | `list` panes in this window |
| `close` | `close <target>` kill the session | `close [-l LABEL]` kill pane + reconcile markers |
| `reconcile` | — | `reconcile` clear an orphaned ⚓ anchor |

`<target>` = `<alias>` (default session `helm`) or `<alias>:<label>` (session `helm-<label>`); `local`/`local:<label>` = an escape-hatch session on the operator's machine (see *panes, copy-mode, the `local` hatch*). Pane default tag is `helm` (`-l` omitted); `-l logs` → tag `helm-logs` (its own pane). One surface per shell: anything with a host → `helm shell`; in-window panes → `helm pane`. A viewport is read-only to me — driving it is banned; I drive the remote via `helm shell`. `send`/`key`/`read`/`run`/`open` auto-create the session/pane on first use (1–2s; first read may show an empty pane/MOTD — one more cycle); `wait`/`watch` resolve-only (never create).

---

## prefer `run` for a command whose result I just need

For a **non-interactive command**, reach for `run` first — one call sends the command, waits event-driven (host-side signal; returns the instant it's done, no polling), and returns **only that output + `exit: N`**. No read-then-send loop; the exit code is the answer.

```sh
SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell run web "doas rcctl restart httpd"
# → (command output)
# exit: 0
```

`run` is single-line, non-interactive: a pane in a pager/editor/mid-command is reported `busy` → fall back to `read`/`send`/`key`; a command that exits the shell (`exit`) → session gone; newlines rejected. `run` does NOT replace read-then-send for interactive/risky work (next section).

**Long `run` (may exceed ~30s) — park in a background shell, never poll `read`.** `run` blocks until done (default `--timeout 30`). For a build/migration/upgrade, move the whole helm call through the harness background-shell tool (`background_bash` in pi; `run_in_background` in Claude Code) with a generous timeout — completion returns deterministically with output + `exit: N`, no `send`+`read`-polling loops. Same for `helm pane run` locally.

---

## the read-then-send discipline (interactive / uncertain state)

When I'm **not** using `run` — interactive program, uncertain state, risky mutation the operator should watch land — **never `send` blind. `read` first, judge, then act.**

```sh
SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell read vps1 -n 50
```

Look for:
- **Clean prompt** (`$ `, `# `, `<user>@host ~ %`) → safe to send.
- **Mid-command** (no trailing prompt, output streaming) → wait or abort; don't pile on.
- **Interactive program** (vim, less, pager) → stop, or drive with `key` (below). Don't fire a shell line into it.
- **Password prompt** (line ending in `:` with `password`/`passphrase`) → stop, tell the operator — **only if it's the LAST non-empty line**; a fresh prompt below means it was already satisfied, proceed.
- **Failed command, undecided** → read more, check with the operator before fixing.

One logical command per `send`, then `read` again before the next. Latency is a feature — it keeps the operator in the loop.

---

## verifying success (send/read path)

`send` returns instantly; it does not wait for the remote command. After sending, **`wait` for completion — never sleep-poll `read`.** `helm shell wait <target>` / `helm pane wait [-l LABEL]` blocks (host-side, zero tokens while waiting) until the session's foreground returns to a prompt: exit 0 done / 124 still busy at `--timeout` (default 60s; wait again or peek) / 1 gone.

```sh
helm shell read web -n 30                       # check state
helm shell send web "doas rcctl restart httpd"
helm shell wait web                             # block until back at a prompt
helm shell read web --delta                     # only the new output
```

`send`'s exit code only confirms keystrokes reached tmux, never that the command succeeded; `wait` reports "back at a prompt", never the command's exit code (nothing is wrapped — that's the trade that keeps interactive flows safe). Judge success from `read --delta`. `send` → `wait` → `read --delta` is the standard pattern for anything `run` refuses (interactive prompts, multi-line, mid-something panes). For expected-long waits, park the whole `wait` in a background shell like a long `run`.

**Waiting on a specific line, not just idle — `watch --match`.** When the tell is a known marker (a deploy's `done`, a server's `Listening on`, an error banner), `helm shell watch <target> --match "REGEX"` / `helm pane watch [-l LABEL] --match …` blocks the same way but returns the instant a line matching the extended regex (`grep -E`) appears in output produced *after* the watch started (pre-existing text can't trigger it): exit 0 matched / 124 no match at `--timeout` / 1 gone. `--idle` is the default predicate and is exactly `wait`; pass exactly one predicate per call. A long `watch` parks in a background shell like a long `wait`/`run`.

**Polling? Use `read --delta`.** Any second-and-later read — confirming a `send`, watching a long command, tailing a log pane — should be `--delta`: returns only lines since my previous `--delta` read, so old scrollback never re-enters context. First delta (or after `clear`/a TUI redraw) falls back to a full read and reseeds — it says so on stderr. `-n` caps a huge delta. Plain `read` is for the *first* look at an unknown pane and for TUIs; `--delta` doesn't re-show lines rewritten in place (progress bars).

---

## driving TUIs with `key`

`send` types a line + Enter — can't drive a full-screen program. `key` sends raw tmux key specs with **no Enter**, so I can operate vim, htop, a menu, a pager — **and on a remote host over ssh** (which nothing else here can do):

```sh
helm shell key vps1 Down Down Enter     # menu navigation
helm shell key vps1 C-c                 # interrupt
helm shell key web Escape : w q Enter   # save+quit vim
helm pane key C-c                       # local pane
```

Key specs: `Up Down Left Right`, `Enter`, `Escape`, `Tab`, `BSpace`, `C-c`, `M-x`, `F1`…`F12`, `Space`, `PageUp`/`PageDown`; multiple keys sent in order. Literal text (not key names) uses `send` (which presses Enter) — there's no literal-without-Enter mode, so a literal word like "Enter" can't be typed via `key`. `read` after driving to see the screen.

---

## doas / sudo persistence: poll before asking

doas/sudo cache credentials **per-tty** (a tmux pane is one tty): **5 min for doas, ~15 for sudo, sliding** (every successful doas/sudo re-arms the full window). Steady doas traffic keeps a pane unlocked indefinitely; a gap past the window lets it lapse. The operator typing their password in the attached viewport arms the *same* pty I drive (same pane = same tty), which is why persistence carries to my sends.

**The rule: never preemptively warn about a password. Run the doas/sudo command, then judge from actual scrollback.** With `run`, the `exit: N` + output tell the story; with `send`, `read` and decide — I can't reliably clock the window between my own calls, and external activity can expire it. Two inputs sharpen the prior (never replace the read): the operator's word ("doas is good" → treat as armed, still confirm) and time elapsed in-pane (many minutes since last doas → expect a possible re-prompt; steady traffic → expect persistence).

After a `doas` `send`, three states (read to disambiguate): **persistence hit** (output or fresh prompt, no `password:` line) → continue; **prompt sitting open** (last non-empty line matches `doas \(.*\) password:` / `[sudo] password for …:`, no prompt below) → stop, tell the operator to type it into the viewport, poll until it clears; **mid-execution** (neither, long command streaming) → poll again, don't assume password. Poll ~1s after send, again 2–3s later if ambiguous. Never announce a password prompt until I've seen one. (Probe without prompting: `doas -n true` exits 0 if armed, nonzero+silent if not.)

**Pre-arm only when root is the whole job.** For a plainly root-heavy sequence (a deploy, service reconfigure, batch of `rcctl`/`pkg_add`), pre-arm once up front: with the viewport open, `send "doas true && echo armed"`, let the operator type the password, `wait` for the prompt, `read --delta` to confirm `armed`. Then the rest runs clean — a plain `run` returns `exit: N` with no prompt. For one-off/uncertain doas, don't pre-arm (it just nags); run and handle any prompt reactively.

---

## writing root-owned state over a shared pane

A stray operator keystroke can garble a `send` mid-command (the viewport is a live tty they also type into). If the garble lands in a pipe that *replaces* root state — `… | doas crontab -`, `… | doas tee /file` — it can clobber the file. Prefer idioms that can't wipe:

- **Append:** `printf '%s\n' "$line" | doas tee -a /path` — `-a` appends, so even a garbled send adds a stray line, not a blank file. Bare `doas tee /file` and any `… | doas <replace-from-stdin>` overwrite — avoid under collision risk.
- **Replace-only interfaces** (e.g. `crontab`, no append): stage a temp first (`doas crontab -l > /tmp/f; …edit…; doas crontab /tmp/f`), never pipe live into `crontab -`.

Keep root-mutating sends short; ask the operator to hold typing during the doas step. Confirm from a clean `helm exec` (mtime/content), never the possibly-garbled scrollback.

---

## panes, copy-mode, the `local` hatch

- **`helm pane` requires `$TMUX_PANE`** (I'm inside the operator's tmux). If unset, it says so — offer `local:<label>` or a headless remote. Auto-split, never adopt a stray pane (it could be running anything). Default to ONE pane; spawn a labeled one (`-l logs` for a parked `tail -f` I read without blocking the main pane) only when asked; close leftovers with `helm pane close -l <label>`. Sizing: 50/50 right by default; `--below` horizontal, `--size N` specific. `close` kills the pane and reconciles the window's ⚓ markers unconditionally (drops `@helm_here` + border when no helm pane remains, even if the labelled pane was already gone).
- **Copy-mode (silent failure):** if `send`/`key` appears to deliver but nothing executes (repeated `read` shows the same lines, no prompt at the bottom), the pane is in tmux copy-mode (operator scrolled up). Sends still land at the bottom but the view is frozen. Fix: ask the operator to press `q`/`Esc`, or close+reopen the pane (`helm shell close`/`open -d`, or `helm pane close`/`open`) — loses scrollback, often a feature (purges leaked secrets).
- **`local[:label]`** is a separate tmux *session* (not a pane) on the operator's machine — only for a shell that must outlive the current window (park a build/server, close the window, lives until reboot). Driven via `helm shell` (the `local` alias skips ssh); never the default. Viewing it nests clients: `tmux switch-client -t helm` (jumps the whole client; `switch-client -l` returns), or `TMUX= helm shell open local:<label>` in a new window (the `TMUX=` lets the nested client attach).

---

## self-attaching a terminal (opt-in only)

Remote targets normally don't need this — the auto-viewport already gives the operator eyes. Only when the operator **explicitly asks in that message** ("attach a terminal", "open it for me", "pop up the session") may I attach one — never automatic, never a standing permission. Inside tmux: `helm pane view <target>` (or `tmux new-window -d -n <target> "SSH_AUTH_SOCK=… helm shell open <target>"`). Outside tmux (needs `DISPLAY`, usually `:0`), backgrounded: `DISPLAY=:0 st -e helm shell open local:diag &`. Attach exactly the named target, then confirm; if no tmux and no `DISPLAY`, say so and ask the operator to run `helm shell open <target>`.

---

## visibility hand-off

Narrate intent before sending — **two sentences max** ("About to restart httpd on web. Reading state first.") — so the operator can interrupt before keys land. After a multi-step session, one-sentence summary of what's done / still open.

---

## when NOT to use this skill

- Operator wants a quick one-shot output in our conversation → `helm exec <alias> <cmd>`.
- No session/pane open AND the operator hasn't asked for one → don't create speculatively; ask first.
- A password prompt **actually appears in scrollback** → stop, tell the operator. Don't preempt (see *doas persistence*).
- Destructive ops (`rm -rf`, `doas pkg_delete -X`, dropping a DB, force-push) → narrate intent, wait for explicit go-ahead. `run` does not bypass this.

---

## secrets in scrollback

`read` (and `run`'s captured output) pulls the visible pane verbatim into my context/transcript — treat every read as a potential leak.

1. **Never print a secret directly.** Check existence by count: `doas grep -c '^FOO_TOKEN=' /etc/<svc>/env` → `0`/`1`, not the value.
2. **Never inline a secret in `send`/`run`** — keystrokes are visible in the pane and my next read. If the operator must enter a value, they type it in their attached terminal.
3. **Filter every third-party API response before bytes hit the pane** (many return secrets in `GET` payloads), in the same pipeline:
   ```sh
   curl … | sed -E 's/"(Password|Token|Secret|ApiToken|ServerToken|AccountToken|ClientSecret|PrivateKey|BasicAuthPassword|HttpAuthPassword)":"[^"]*"/"\1":"<REDACTED>"/g' | tee /tmp/resp.json
   ```
   Filtering after the fact doesn't help — the unredacted bytes already passed through the pane.
4. **If a secret leaks anyway, alert the operator and offer to rotate** — don't silently move on. They decide rotate-now vs batch-at-session-end.
5. **Closing + reopening a pane/session purges scrollback** — a cheap drop for accumulated secrets.

---

## ssh-agent bridge (remote targets only)

`local` and `helm pane` skip ssh — no agent setup. For ssh aliases, my Bash calls don't inherit `SSH_AUTH_SOCK` from the operator's terminals (different process tree). The operator runs a persistent agent at a fixed path; I prefix every remote `helm shell` call (the harness resets env between Bash calls, so exporting doesn't stick):

```sh
# operator (once per boot):  ssh-agent -a /tmp/<user>-ssh-agent.sock && SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock ssh-add ~/.ssh/<vps-key>
SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell read vps1 -n 50
```

`helm pane view` embeds the socket into the viewport's pane command automatically (it reads my current `SSH_AUTH_SOCK`). On `Permission denied (publickey)` / `Could not open a connection to your authentication agent`: ask the operator to verify `ls /tmp/<user>-ssh-agent.sock` and `SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock ssh-add -l` lists the VPS key; if empty, re-run the spawn sequence.
