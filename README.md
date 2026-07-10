# helm

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 2024](https://img.shields.io/badge/edition-2024-dea584.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/)
[![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20OpenBSD-lightgrey.svg)](#)
[![Status](https://img.shields.io/badge/status-v0.4.4-orange.svg)](#)

A Rust CLI that lets an AI agent and a human drive the *same* tmux session — local or ssh-remote — with the discipline baked in so neither side breaks the other. The agent can run a command and read its exit code in one call (returning the instant it finishes — completion is event-driven, not polled), block until an interactive flow is back at a prompt, poll a long job with delta reads that never re-ingest old output, send raw keys to drive a full-screen TUI, or split a pane right inside your own tmux window. Plus a handful of read-only verbs for inspecting the fleet of hosts those sessions live on.

**Status:** v0.4.4. Two shared-shell surfaces — `helm shell` (a tmux session on any ssh host, or locally) and `helm pane` (a pane in your own tmux window) — plus one-shot `helm exec`, the fleet-inspection verbs, and an append-only audit log. Linux + macOS + OpenBSD supported.

![helm — inspecting a fleet, splitting a drivable pane in your own tmux, running a command for its output and exit code, and reading the audit log — all from one CLI](docs/demo.gif)

## Why

Agent shells today are usually one-shot: Claude Code's `Bash` tool ([#9881](https://github.com/anthropics/claude-code/issues/9881), [#4319](https://github.com/anthropics/claude-code/issues/4319)) and most peers spawn fresh subprocesses, lose cwd, hang on interactive prompts, and never let the human watch the agent type. Giving the agent a real persistent tmux session fixes all of that — and in 2025 a cluster of projects converged on the idea ([TmuxAI](https://github.com/alvinunreal/tmuxai), [mitsuhiko/agent-stuff](https://github.com/mitsuhiko/agent-stuff/blob/main/skills/tmux/SKILL.md), [tmux-mcp](https://github.com/bnomei/tmux-mcp), [Hiren Patel's tag-teaming pattern](https://patelhiren.com/blog/tag-teaming-claude-code-with-ai-agent/)).

What's missing is a **plain CLI binary form factor** (no MCP server registration, no skill registry hop, no raw `tmux send-keys` in the agent's mouth) wrapped around an **encoded etiquette** the agent reads before it touches the shell. That's helm.

## Philosophy

- **A sidekick, not a swarm.** One agent and one human in the same shell, watching each other. Orchestrators spawn N agents in N panes; helm deliberately doesn't.
- **A plain binary, not a server.** No MCP registration, no daemon, no protocol. Anything that can run a shell command can drive helm — and everything that does leaves a record in the same audit log.
- **The human sees everything.** The agent types into the pane you're attached to, narrates before it sends, and every call lands in an append-only log. Mutating verbs refuse without `--yes`.
- **Waiting happens in the binary, not in the model.** Agent context is the scarce resource, so every verb is shaped to spend the minimum of it. `run` returns one command's output + exit code in a single ssh round-trip, waking the instant the command signals completion (`tmux wait-for`, not a capture-and-grep loop). `wait` blocks host-side until the pane is back at a prompt. `read --delta` returns only lines it hasn't returned before. An agent that would otherwise sleep-poll `read` — re-ingesting the same scrollback every pass — instead makes one blocking call and reads once.
- **Etiquette as an artifact.** The discipline isn't a hope, it's a file: the skill ships in this repo and the agent reads it before touching the shell.
- **Refusal over recovery.** `run` refuses a busy pane rather than fire into a running program; passwords are refused outright; mutations refuse without an explicit flag. Errors you never make don't need cleanup.

## The discipline

Three rules every interaction obeys. The agent never breaks them, the human doesn't either:

1. **Read before send — or let `run` do the checking.** For interactive or risky work the agent reads first (`helm shell read <target>`) to confirm the pane is at a clean prompt — not mid-command, not inside `vim`, not staring at a password prompt. For a plain non-interactive command it uses `helm shell run`, which verifies the pane is at a shell prompt, runs the command, and hands back its output plus `exit: N` — refusing outright if the pane is busy. Either way, helm never fires a line into a running program by accident.
2. **Narrate intent before sending.** Two sentences max, in chat, before any keystrokes land. The human has time to interrupt before anything visible happens in the shell.
3. **Refuse to type passwords.** When `read` shows a `password:` or `passphrase:` line, the agent stops and tells the operator. The human answers in their own attached tmux pane.

Hand the agent the skill at [`.claude/skills/helm/SKILL.md`](.claude/skills/helm/SKILL.md). It encodes the three rules above plus the read-then-send loop, label conventions for parallel work, and a `ssh-agent` socket bridge pattern.

## Quickstart

```sh
# macOS — tap-qualified to avoid the Kubernetes Helm collision
brew tap crodorg/helm
brew install crodorg/helm/helm

# Linux / OpenBSD
git clone https://github.com/crodorg/helm
cd helm && cargo build --release
ln -s "$PWD/target/release/helm" ~/.local/bin/helm
```

> **Note on the name.** `brew install helm` (unqualified) gets you the Kubernetes package manager — a completely different `helm`. Always tap-qualify: `brew install crodorg/helm/helm`. If you also use Kubernetes Helm, only one can own `/opt/homebrew/bin/helm`; pick whichever you reach for more, or `brew link --overwrite` to swap.

Open a shared tmux session against any host (or your own machine via the reserved `local` alias):

```sh
helm shell open web              # attach (creates if missing)
helm shell open -d web:deploy    # ensure exists, stay detached
helm shell run  web 'uptime'     # run one command, get its output + exit code
helm shell wait web              # block until the session is back at a shell prompt
helm shell read web              # capture current pane scrollback
helm shell send web 'cd /srv'    # type a line + Enter; shell state persists
helm shell key  web C-c          # send a raw key (no Enter) — drive a TUI
helm shell list web              # list helm-* sessions on web
```

`<target>` is `<alias>` or `<alias>:<label>`. The reserved alias `local` short-circuits ssh and uses the operator's own tmux server. For interactive use, `helm open <target>` is shorthand for `helm shell open <target>` — `helm open web`, `helm open web:deploy`, or any IP / `~/.ssh/config` host attaches a persistent shell in one word.

When the work is **local and you're already in tmux**, `helm pane` splits a pane right in your current window instead — same verbs, no ssh:

```sh
helm pane open                   # split a drivable shell pane here
helm pane run 'cargo test'       # run one command, get output + exit code
helm pane wait                   # block until the pane is back at a shell prompt
helm pane view web               # read-only viewport onto a remote session
helm pane key C-c                # raw keys into the local pane
```

## The surface

Two surfaces, the same verbs. **`helm shell`** drives a tmux *session* on an ssh host (or locally via `local`); **`helm pane`** drives a *pane* in the operator's own tmux window. The agent never touches raw `tmux`.

| `helm shell <target> …` | What it does |
|---|---|
| `open <target>` | Attach this terminal to the session. Creates if missing. |
| `open -d <target>` | Same, but stays detached — pre-create a session you intend to drive. |
| `run <target> <cmd>` | Run one non-interactive command; print its output + `exit: N` in a single ssh round-trip. Completion is event-driven (`tmux wait-for`) — the call returns the instant the command finishes, with a watchdog backstopping shell death and timeouts. Refuses if the pane is busy. |
| `wait <target> [--timeout S]` | Block until the session is back at a shell prompt (exit 0 done / 124 still busy / 1 gone) — the poll runs host-side in the same single round-trip. The sentinel-free companion to `run` for interactive flows (password prompts, multi-line input): `send`, `wait`, then `read --delta`. |
| `send <target> <text>` | Type the line + Enter. Lands in the pane the human is attached to. |
| `key <target> <key…>` | Send raw tmux key specs (`Up`, `C-c`, `Escape`) with no Enter — drive a full-screen TUI, including over ssh. |
| `read <target> [-n N]` | Capture scrollback from the active pane (trailing blanks trimmed; `--raw` keeps them). Called *before every interactive send*. `--delta` returns only lines new since the previous `--delta` read — repeated checks never re-ingest old output. |
| `list <alias>` | List `helm-*` sessions on that alias's tmux server. |
| `close <target>` | Kill the session. |

`helm pane` mirrors `open / run / wait / send / key / read / list / close` for a pane in the current window, plus `view <target>` — a **read-only viewport** onto a remote `helm shell` session so the human watches the agent work live (the agent drives the remote through `helm shell` and never types into the viewport). It needs `$TMUX_PANE` (helm must be running inside the operator's tmux); local shell work with no host named defaults here.

Both differ fundamentally from `helm exec <alias> <cmd>`, which is one-shot and stateless. A shell or pane retains cwd, env, history, and in-progress prompts across calls; `helm exec` runs ssh once, streams the output, records it, and exits. Use `run` when you just need a command's result and exit code; use `exec` when the output should stream straight back into the conversation with no session to keep.

## In practice

helm is daily-driven, not a demo. The reference deployment is a small personal fleet — OpenBSD VPSes carrying web, mail, and relay duty, a Linux box, a Mac — plus the local dev machine, driven by coding agents (Claude Code and peers) through the shipped skill while the operator watches from tmux:

- **Local pane work is the highest-traffic surface.** The agent splits a pane in the operator's own window for builds, test runs, and anything needing `doas`/`sudo` — the human answers the password prompt in the same pane the agent is working in.
- **Remote sessions carry the ops work** — deploys, migrations, service restarts, log tails — with `helm pane view` giving the operator a live read-only viewport onto what the agent is doing over ssh.
- **The agent-efficiency verbs earn their keep on long jobs.** A build or test suite becomes `run` (returns at completion with the exit code), an interactive flow becomes `send` → `wait` → `read --delta`, and repeated progress checks stop re-reading the same scrollback.
- **Fleet verbs are the morning check**: `svc`, `ports`, `logs`, `vultr` — what's running, what's listening, what it costs.

## Prior art

helm isn't the first tool to hand an agent a persistent shell. [mitsuhiko/agent-stuff](https://github.com/mitsuhiko/agent-stuff/blob/main/skills/tmux/SKILL.md) and [bnomei/tmux-mcp](https://github.com/bnomei/tmux-mcp) wrap tmux behind a skill or an MCP server; [TmuxAI](https://github.com/alvinunreal/tmuxai) ships its own binary with a separate execute pane. Orchestrator-class projects ([Tmux-Orchestrator](https://github.com/absmartly/Tmux-Orchestrator), [awslabs/cli-agent-orchestrator](https://github.com/awslabs/cli-agent-orchestrator), [claude_code_agent_farm](https://github.com/Dicklesworthstone/claude_code_agent_farm), [amux](https://github.com/mixpeek/amux)) solve a different problem: spawning N parallel agents, each in its own pane.

helm is a sidekick, not a swarm — one shared pane, a plain CLI binary, and an etiquette the agent reads before it types.

## Audit log

Every `helm exec`, every `helm shell {open,run,wait,send,key,read,list,close}`, and every `helm pane` action (recorded with alias `pane`) writes one JSON line to:

- Linux/BSD: `$XDG_STATE_HOME/helm/activity.jsonl` (default `~/.local/state/helm/activity.jsonl`)
- macOS: `~/Library/Application Support/helm/activity.jsonl`

`helm activity` prints the most recent records — time, exit status, kind, target, command. Each record also carries a privilege-escalation flag, set when a `doas` / `sudo` token appears at the start of a command or after `|` / `&&` / `;`. The log is append-only and agent-agnostic — Claude Code, Cursor, Aider, a bash one-liner, all write the same record.

```sh
helm activity -n 50
tail -f ~/.local/state/helm/activity.jsonl | jq .
```

## Fleet inspection

helm also inspects the fleet those sessions live on. Each verb reads over ssh and prints a table — add `--json` for machine output:

| Verb | What it shows |
|---|---|
| `helm ls` | configured + `~/.ssh/config` hosts |
| `helm show <host>` | one host's detail + linked businesses |
| `helm svc <host>` | service inventory (rcctl / systemctl / launchctl) |
| `helm ps <host> [-n N]` | top processes by CPU |
| `helm ports <host>` | listening sockets |
| `helm vultr` | Vultr instances + monthly cost |
| `helm logs <host> [key] [-f]` | list or tail a host's logs |
| `helm history [<id>] [-n N]` | recent `helm exec` history (SQLite); `<id>` shows one run's transcript |
| `helm activity [-n N]` | recent agent audit log |

Two mutating verbs are operator-only. They refuse without `--yes`, so they never sit on the un-gated agent surface:

```sh
helm vultr reboot|halt|start|snapshot <id> --yes
helm run <key> <host> --yes        # run a configured [[shortcuts]] command on a host
```

### Per-host init system (`helm svc`)

`helm svc` is the one verb whose command diverges across OSes. Tag each host:

```toml
[[hosts]]
ssh_alias = "web"
os = "openbsd"     # openbsd | linux | macos — defaults to openbsd
```

| `os`      | Command helm fires                                                               |
|-----------|----------------------------------------------------------------------------------|
| `openbsd` | three parallel `doas -n rcctl ls {on,started,failed}`                             |
| `linux`   | `systemctl list-units --type=service --all --no-legend --plain --no-pager`        |
| `macos`   | `launchctl list` (user-domain services only)                                     |

`linux` covers any systemd distro. Non-systemd Linux (Void/runit, Alpine/OpenRC) isn't recognized yet. OpenBSD `rcctl ls started|failed` needs root — add three `permit nopass` lines per OpenBSD host:

```
permit nopass <user> cmd rcctl args ls on
permit nopass <user> cmd rcctl args ls started
permit nopass <user> cmd rcctl args ls failed
```

### Verbs that need an external CLI or key

A few verbs depend on something beyond ssh:

- `helm vultr` needs `$VULTR_API_KEY` (read-only listing; the `reboot/halt/start/snapshot` mutations also use it).

### A "business" in helm

A "business" is helm's noun for **any named thing with a domain** — a personal site, side project, OSS landing page, a blog. `[[businesses]]` entries link a domain to the host it runs on; `helm show <host>` lists the businesses on a host (flagging any linked to a Stripe/Mercury account). Minimum entry:

```toml
[[businesses]]
name = "my-blog"
primary_domain = "blog.example.com"
host = "web"
```

## Configuration

If you only use `helm shell` + the agent skill, you don't need a `config.toml`. For the fleet verbs, copy and edit:

```sh
cp config.example.toml ~/.config/helm/config.toml
```

Loaded in order: cwd → platform config dir (`~/.config/helm/` Linux/BSD, `~/Library/Application Support/helm/` macOS) → `$XDG_CONFIG_HOME/helm/`. helm prints the resolved path to stderr when it loads config. `config.toml` is gitignored.

Hosts come from `[[hosts]]` entries *and* `~/.ssh/config` Host blocks (wildcards skipped). For ssh-config-discovered hosts that aren't OpenBSD, tag the OS:

```toml
[ssh_config.os]
laptop    = "macos"
linux-vps = "linux"
```

Every tmux invocation helm makes (remote and local) carries the flags from `tmux_flags`, which defaults to `["-u"]` — forces UTF-8 so unicode renders even when the remote locale doesn't advertise it. Set `tmux_flags = []` to disable, or list your own (`["-u", "-2"]`). This also applies to `helm shell`, which otherwise reads no config.

## SSH

helm shells out to the system `ssh` binary. Requirements:

- Every `ssh_alias` must be a Host entry in `~/.ssh/config`.
- `ssh-agent` must be loaded — helm has no key-passphrase UI.
- `ProxyJump`, `IdentityFile`, `Port`, etc. live in `~/.ssh/config`, not in helm.

`helm auth` checks this explicitly: it runs `ssh-add -l`, fingerprints each `IdentityFile` referenced by your hosts, and exits 0 if every key is loaded, non-zero otherwise — wire it into a login shell or a `doas`/`sudo` wrapper. `helm auth --load` shells out to `ssh-add <path>` for each missing key (it prompts for the passphrase) and re-checks.

## Remote shell environment

`helm shell` opens a **login shell** on the remote host, so that host's startup files (`~/.profile`, `~/.zprofile`, `~/.bashrc`, …) run at session start — exactly as on a normal login. helm augments only `$PATH` (so brew / MacPorts binaries resolve in a non-interactive ssh shell); every other variable in the session comes from the host's own dotfiles, not from helm.

On a mixed fleet that makes an unguarded Linux-ism in a **shared** dotfile a footgun — it runs on every host, including the ones it doesn't fit. The classic is `XDG_RUNTIME_DIR`:

```sh
# Linux convention — errors on macOS (no writable /run) and OpenBSD
XDG_RUNTIME_DIR="/run/user/$(id -u)"
mkdir -p "$XDG_RUNTIME_DIR"        # mkdir: /run: Read-only file system
```

Guard host-specific setup on `uname` so it only fires where it belongs:

```sh
if [ "$(uname)" = Linux ]; then
    XDG_RUNTIME_DIR="/run/user/$(id -u)"
    [ -d "$XDG_RUNTIME_DIR" ] || { mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"; }
fi
```

helm itself never sets `XDG_RUNTIME_DIR` (or any host env beyond `$PATH`) — the session is the host's own shell, so a fix like this lives in the host's dotfiles, not in helm.

## Limitations

Honest edges, so you don't discover them mid-session:

- **`wait` reports "at a prompt", never an exit code.** It exists for exactly the interactive flows `run` refuses (password prompts, multi-line input), and wrapping those in a sentinel is what's unsafe — so it watches the pane's foreground command instead. `run` remains the exit-code path.
- **Busy detection is heuristic.** `run`'s busy-guard and `wait` both compare `#{pane_current_command}` against a list of known idle shells; an exotic shell name defeats it.
- **`read --delta` reseeds on scrollback disruption.** `clear`, a full-screen TUI redraw, or history trimming invalidates the cursor; the next delta read falls back to a full read and starts over.
- **Event-driven `run` assumes stock tmux `wait-for` semantics** (verified on 3.6). On hosts where the signal can't land, the engine degrades to a 0.5 s poll — never a hang — and a watchdog covers shell death and timeouts either way.
- **`helm svc` knows three init systems** — rcctl, systemd, launchctl (user domain only). Non-systemd Linux (runit, OpenRC) isn't recognized; OpenBSD needs the three `doas permit nopass` lines above.
- **Hard requirements.** tmux on every host you open a shell on; system `ssh` with `ssh-agent` loaded (helm has no passphrase UI); `helm pane` only works from inside the operator's own tmux (`$TMUX_PANE`).
- **Not an orchestrator.** One shared session or pane per target — no N-agent spawning, no scheduling. That's the point, but if you want a swarm, see the orchestrator projects under Prior art.
- **No Windows.**

## Layout

```
src/
├── main.rs               clap dispatch — routes each verb to its handler
├── args.rs               clap command definitions
├── shell.rs              `helm shell` verbs (open/run/send/key/read/list/close)
├── pane.rs               `helm pane` verbs — drivable panes + viewports in the operator's window
├── runcmd.rs             the run-sentinel engine: send a command, capture its output + exit code
├── tmux.rs               thin tmux verbs — ensure_session, send-keys, capture, flags
├── cli/                  read verbs (ls/svc/ps/…) + the gated mutations
├── activity.rs           append-only JSONL audit log
├── config.rs             TOML loader, ssh-config merge
├── history.rs            SQLite-backed exec history
├── mosh.rs               transport choice (mosh vs ssh) for the attach path
├── vultr.rs              Vultr API over curl, for the vultr verb
├── ssh/
│   ├── sshconfig.rs      ~/.ssh/config parser
│   ├── agent.rs          ssh-agent fingerprint diff
│   ├── collect.rs        per-OS service / process collectors
│   └── run.rs            spawn ssh -tt, mpsc stream, password-prompt heuristic
└── inventory/            services / processes / ports parsers
```

## Testing

```sh
make check          # the full gate CI runs: fmt, clippy -D warnings, tests, file-size cap, coverage ratchet
cargo test          # just the tests
```

Tests are inline `#[cfg(test)]` modules — pure parsers, render functions, and the run-sentinel/arg-parsing logic, exercised without live ssh or tmux. The shell-out layer is kept thin over that tested core.

## License

MIT. See `LICENSE`.
