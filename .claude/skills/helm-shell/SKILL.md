---
name: helm-shell
description: "Drive a persistent VPS-side tmux shell that the operator and I can both see in real time. Sessions live on the remote VPS, survive helm/network/laptop restarts, and are reachable via `helm shell` CLI subcommands. Use this when the operator asks me to run remote commands they want to watch live, when investigating across multiple commands that share shell state (cwd, env, history), or when they explicitly tell me to act on a host they have an open shell to. MANDATORY TRIGGERS: 'send to helm', 'put this in the shell', 'run in my helm shell', 'watch me run X via helm', 'use helm-shell'. STRONG TRIGGERS: any request to act on a remote host where I observe a `helm shell` session is already open (via `helm shell list`) and the request is shell-stateful. SKIP when the request is genuinely one-shot and stateless — `helm exec` or direct ssh is simpler."
---

# helm-shell

A bridge skill. The operator runs a persistent tmux session ON the remote VPS (via `helm shell`). They watch it live by attaching from any terminal. I drive it from the side: read its scrollback, type commands, never blind. The operator can intervene at any moment — passwords, Ctrl-C, anything — because it is their tmux session.

This is fundamentally different from `helm exec`:

- `helm exec` = one-shot, output streams back to my conversation, no shell state retained.
- `helm shell` = stateful long-lived shell, output renders in the operator's tmux pane (and I can scrape it), shell state (cwd, env, history, in-progress prompts) persists.

Use `helm shell` when the operator wants to *watch* me work or when shell state matters. Use `helm exec` otherwise.

---

## the four primitives

All commands are invoked via the Bash tool.

```sh
helm shell list <alias>                    # which helm-* sessions exist on this VPS
helm shell open -d <target>                # create the session detached, no attach
helm shell read <target> [-n LINES]        # scrape scrollback (default 1000)
helm shell send <target> "<text>"          # type a line (auto-Enter)
```

`<target>` is `<alias>` (default session `helm` on that VPS) or `<alias>:<label>` (session `helm-<label>`).

The operator-facing command is `helm shell open <target>` — that attaches *their* terminal. I never call this. I only call the headless variants.

---

## the read-then-send discipline

**Never `send` blind.** Always `read` first, judge the shell state, then act.

```
helm shell read <alias> -n 50
```

Look for:

- **At a clean prompt** (e.g. `$ `, `# `, `user@host ~ % `) → safe to send.
- **Mid-command** (no trailing prompt; output still streaming or last line not a prompt) → either wait or abort what's running first. Do not pile new commands on top.
- **Interactive program** (vim, less, pager) → stop. Tell the operator the session is in vim/less and ask whether to send keys, wait, or have them switch out first.
- **Password prompt** (line ending in `:` containing `password` / `passphrase`) → stop. Tell the operator, do not type. They answer it in their attached tmux.
- **Failed command, undecided next step** → read more context, then check with operator before sending a fix.

Send one logical command at a time. Then `read` again to see the result before sending the next. Latency is a feature here — it's how the operator stays in the loop.

---

## verifying success

`send` returns instantly; it does not wait for the remote command to finish. After sending, sleep briefly (Bash tool's natural latency is usually enough) and `read` again to see the result. For long-running commands, poll `read` until you see the next prompt.

Example pattern:

```sh
helm shell read <alias> -n 30            # check state
helm shell send <alias> "doas rcctl restart httpd"
helm shell read <alias> -n 10            # confirm exit / no errors
```

Do not assume success based on `helm shell send` exit code — that only confirms the keystrokes were delivered to tmux, not that the remote command succeeded.

---

## creating sessions

If `helm shell read` reports the session doesn't exist, `send` and `read` will create it automatically (the tmux session is auto-created on first use). That spawn-and-attach takes 1-2 seconds; the first `read` may show an empty pane or the login MOTD. Wait one more `read` cycle before judging.

To pre-create a session quietly: `helm shell open -d <target>`. Use this if you want the session warm before issuing commands.

---

## multi-pane workflows

For a host where the operator wants me working in one pane while they work in another (or while a long-running command stays in its own pane), use labels:

- `<alias>` = the default shared shell
- `<alias>:deploy` = a separate pane for deploys
- `<alias>:logs` = a pane I park a `tail -f` in so I can read it without blocking the main shell

Each `:label` creates a separate remote tmux session. Operator attaches to each one in their own terminal/window.

---

## visibility hand-off

The operator's primary view is their attached tmux pane. They see every keystroke I send the moment it lands. They don't need to look at my conversation to track what I did; they look at their pane.

But: I should still narrate intent in conversation before sending. Two sentences max. "About to restart httpd on <alias>. Reading state first." Then execute. This gives the operator time to interrupt if my intent is wrong, even before the keys land in their tmux.

After a multi-step shell session, summarize what happened in one sentence — what was done, what's still open. The operator may have stepped away; the summary lets them catch up without reading the whole tmux pane.

---

## when NOT to use this skill

- The operator wants a quick one-shot output to read in our conversation → use `helm exec <alias> <cmd>` instead.
- The host has no `helm shell` session open AND the operator hasn't asked me to create one → don't create speculatively; ask first.
- Doing anything that requires a password I shouldn't see (sudo prompts, ssh-key passphrases) → stop and tell the operator.
- Destructive operations (`rm -rf`, `doas pkg_delete -X`, dropping a database, force-pushing) → narrate intent and wait for explicit go-ahead before sending.

---

## ssh-agent bridge (required)

My Bash invocations don't inherit `SSH_AUTH_SOCK` from the operator's interactive terminals — different process tree, no shared env. The operator should run a persistent agent at a fixed path so I can reach it:

```sh
# operator (once per boot)
ssh-agent -a /tmp/<user>-ssh-agent.sock
export SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock
ssh-add ~/.ssh/<vps-key>
```

I must prefix every `helm shell` (and bare `ssh`) call with that socket — the harness resets env between Bash calls, so exporting in one call does not stick:

```sh
SSH_AUTH_SOCK=/tmp/<user>-ssh-agent.sock helm shell read <alias> -n 50
```

If a call fails with `Permission denied (publickey)` or `Could not open a connection to your authentication agent`: ask the operator to verify the socket file exists and `SSH_AUTH_SOCK=<path> ssh-add -l` lists the VPS key. If empty, they re-run the spawn sequence above.

The exact socket path and key name are operator-specific. Ask once per project; record it in your conversation memory so future calls use the right prefix without re-asking.

---

## reference: helm shell CLI

For completeness; refer to `helm shell help` on the operator's machine for the live source of truth.

| command | purpose |
|---|---|
| `helm shell open <target>` | operator-attaches a terminal to the remote session (I never call this) |
| `helm shell open -d <target>` | pre-create the remote session detached |
| `helm shell send <target> "<text>"` | type a line into the active pane (auto-Enter) |
| `helm shell read <target> [-n LINES]` | capture the active pane's scrollback |
| `helm shell list <alias>` | list helm-* sessions on the alias's remote tmux server |
| `helm shell close <target>` | kill the remote session |

The session name on the remote VPS is `helm` (no label) or `helm-<label>` (with label). All my interactions go through `helm shell`; I never `tmux` directly.
