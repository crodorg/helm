# helm

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 2024](https://img.shields.io/badge/edition-2024-dea584.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/)
[![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20OpenBSD-lightgrey.svg)](#)
[![Status](https://img.shields.io/badge/status-v0.1-orange.svg)](#)

A Rust CLI that lets an AI agent and a human drive the *same* tmux session — local or ssh-remote — with the discipline baked in so neither side breaks the other. Plus a TUI for managing the fleet of hosts those sessions live on.

**Status:** v0.1. Sessions pane, daemon, audit log, and the four-primitive agent surface ship today. Linux + macOS + OpenBSD supported.

![helm demo — sessions pane + detached-ensure](docs/demo.gif)

## Why

Agent shells today are usually one-shot: Claude Code's `Bash` tool ([#9881](https://github.com/anthropics/claude-code/issues/9881), [#4319](https://github.com/anthropics/claude-code/issues/4319)) and most peers spawn fresh subprocesses, lose cwd, hang on interactive prompts, and never let the human watch the agent type. Giving the agent a real persistent tmux session fixes all of that — and in 2025 a cluster of projects converged on the idea ([TmuxAI](https://github.com/alvinunreal/tmuxai), [mitsuhiko/agent-stuff](https://github.com/mitsuhiko/agent-stuff/blob/main/skills/tmux/SKILL.md), [tmux-mcp](https://github.com/bnomei/tmux-mcp), [Hiren Patel's tag-teaming pattern](https://patelhiren.com/blog/tag-teaming-claude-code-with-ai-agent/)).

What's missing is a **plain CLI binary form factor** (no MCP server registration, no skill registry hop, no raw `tmux send-keys` in the agent's mouth) wrapped around an **encoded etiquette** the agent reads before it touches the shell. That's helm.

## The discipline

Three rules every interaction obeys. The agent never breaks them, the human doesn't either:

1. **Read before send.** Every action starts with `helm shell read <target>` to confirm the pane is at a clean prompt — not mid-command, not inside `vim`, not staring at a password prompt. Blind sends are forbidden.
2. **Narrate intent before sending.** Two sentences max, in chat, before any keystrokes land. The human has time to interrupt before anything visible happens in the shell.
3. **Refuse to type passwords.** When `read` shows a `password:` or `passphrase:` line, the agent stops and tells the operator. The human answers in their own attached tmux pane.

Hand the agent the skill at [`.claude/skills/helm-shell/SKILL.md`](.claude/skills/helm-shell/SKILL.md). It encodes the three rules above plus the read-then-send loop, label conventions for parallel work, and a `ssh-agent` socket bridge pattern.

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
helm shell open mac              # attach (creates if missing)
helm shell open -d mac:deploy    # ensure exists, stay detached
helm shell list mac              # list helm-* sessions on mac
helm shell read mac              # capture current pane scrollback
helm shell send mac 'uptime'     # type a line + press Enter
```

That's the full agent surface. `<target>` is `<alias>` or `<alias>:<label>`. The reserved alias `local` short-circuits ssh and uses the operator's own tmux server.

For interactive use, `helm <target>` is shorthand for `helm shell open <target>` — `helm router`, `helm web:deploy`, or any IP / `~/.ssh/config` host attaches a persistent shell in one word.

## The four primitives

| Command | What it does |
|---|---|
| `helm shell open <target>` | Attach this terminal to the session. Creates if missing. |
| `helm shell open -d <target>` | Same, but stays detached — agent pre-creates a session it intends to drive. |
| `helm shell read <target>` | Capture scrollback from the active pane. Called *before every send*. |
| `helm shell send <target> <text>` | Type the line + Enter. Lands in the pane the human is attached to. |
| `helm shell list <alias>` | List `helm-*` sessions on that alias's tmux server. |
| `helm shell close <target>` | Kill the session. |

`helm shell` is fundamentally different from `helm exec <alias> <cmd>`, which is one-shot and stateless. `helm shell` retains cwd, env, history, and in-progress prompts across calls; `helm exec` runs and exits.

## Prior art

| | CLI binary | Skill (encoded) | ssh-remote | Refuses passwords | Sidekick (shared pane) |
|---|---|---|---|---|---|
| **helm** | ✓ | ✓ read+narrate+refuse | ✓ | ✓ | ✓ |
| [mitsuhiko/agent-stuff](https://github.com/mitsuhiko/agent-stuff/blob/main/skills/tmux/SKILL.md) | — (raw tmux) | ✓ read-before-send | indirect | — | ✓ |
| [bnomei/tmux-mcp](https://github.com/bnomei/tmux-mcp) | — (MCP) | — | ✓ | — | ✓ |
| [TmuxAI](https://github.com/alvinunreal/tmuxai) | own binary | — | — | — | separate execute pane |
| Tmux-Orchestrator class | varies | — | spawn-only | — | — (own panes) |

Orchestrator-class projects ([Tmux-Orchestrator](https://github.com/absmartly/Tmux-Orchestrator), [awslabs/cli-agent-orchestrator](https://github.com/awslabs/cli-agent-orchestrator), [claude_code_agent_farm](https://github.com/Dicklesworthstone/claude_code_agent_farm), [amux](https://github.com/mixpeek/amux)) solve a different problem: spawn N parallel agents each in its own pane. helm is sidekick, not swarm.

## Audit log

Every `helm exec` and every `helm shell {open,send,read,list,close}` writes one JSON line to:

- Linux/BSD: `$XDG_STATE_HOME/helm/activity.jsonl` (default `~/.local/state/helm/activity.jsonl`)
- macOS: `~/Library/Application Support/helm/activity.jsonl`

The TUI's **agent activity** pane (`c` from Browse) tails this file: time, exit status, kind, target, command, output preview. Privilege-escalating commands (any `doas` / `sudo` token at the start of a command or after `|` / `&&` / `;`) get a red `[DOAS]` badge. The log is append-only and agent-agnostic — Claude Code, Cursor, Aider, a bash one-liner, all write the same record.

```sh
tail -f ~/.local/state/helm/activity.jsonl | jq .
```

## Daemon

`helm exec` talks to a control socket. The TUI owns the socket when running; `helm daemon` owns it otherwise. Either way, an agent calling `helm exec` from a separate shell gets the same streamed output, history row, and audit entry.

```sh
helm daemon start    # spawn detached
helm daemon stop     # ask running daemon to exit
helm daemon status   # exit 0 if reachable
```

Coexistence is automatic: opening the TUI shuts down a running daemon; closing the TUI re-spawns one (set `auto_daemon = false` in `config.toml` to opt out). Socket lives at `$XDG_RUNTIME_DIR/helm.sock` (Linux/BSD) or `~/Library/Caches/helm/helm.sock` (macOS).

## Fleet TUI

`helm` with no args opens a TUI for the workflow it was originally built for: managing a small fleet of hosts. Browse hosts, run ad-hoc remote commands with a `doas`/`sudo`-prompt-aware password modal, check service health per init system, tail logs, replay past runs from a SQLite history. Press `S` from Browse for the **sessions pane** — a live table of every active `helm shell` session across the fleet, with `Enter` to attach.

### Keys

Browse:
- `j` / `k` — move; `Enter` — interactive ssh; `q` / `Esc` — quit
- `r` — runner; `s` — services; `S` — sessions; `p` — processes; `l` — logs; `t` — history; `a` — shortcuts; `c` — agent activity
- `?` — in-TUI help for the current pane
- `F5` — reload `config.toml`; `R` — refresh all overlays

Opt-in panes (off by default — see [`[features]`](#optional-panes-features)): `H` health, `v` vultr, `d` dns, `m` money.

### Services pane / per-host init system

The Services pane is the only inventory pane that diverges across OSes. Tag each host:

```toml
[[hosts]]
ssh_alias = "web"
os = "openbsd"     # openbsd | linux | macos — defaults to openbsd
```

| `os`      | Command helm fires                                                              |
|-----------|----------------------------------------------------------------------------------|
| `openbsd` | three parallel `doas -n rcctl ls {on,started,failed}`                            |
| `linux`   | `systemctl list-units --type=service --all --no-legend --plain --no-pager`       |
| `macos`   | `launchctl list` (user-domain services only)                                     |

`linux` covers any systemd distro. Non-systemd Linux (Void/runit, Alpine/OpenRC) isn't recognized yet. OpenBSD `rcctl ls started|failed` needs root — add three `permit nopass` lines per OpenBSD host:

```
permit nopass <user> cmd rcctl args ls on
permit nopass <user> cmd rcctl args ls started
permit nopass <user> cmd rcctl args ls failed
```

### Optional panes (`[features]`)

Side panes that depend on external CLIs / API keys. All default to off:

```toml
[features]
health = false   # H — HTTPS reachability + TLS expiry per business
vultr  = false   # v — Vultr instance overlay (needs $VULTR_API_KEY)
dns    = false   # d — per-business A / AAAA / MX / CAA table
money  = false   # m — Stripe + Mercury balances (needs pp CLIs)
```

Disabled panes are hidden from the Browse keys palette **and** the help overlay. The money pane shells out to `stripe-pp-cli` + `mercury-pp-cli` from [printing-press-library](https://github.com/mvanhorn/printing-press-library); bring-your-own works too (helm just expects a binary on `$PATH` printing the Stripe/Mercury balance JSON shape).

### A "business" in helm

Three optional panes (`health`, `dns`, `money`) iterate `[[businesses]]`. The naming is historical — a business in helm is **any named thing with a domain**: personal site, side project, OSS landing page, a blog. Minimum entry for the health pane:

```toml
[[businesses]]
name = "my-blog"
primary_domain = "blog.example.com"
host = "personal"
```

## Configuration

If you only use `helm shell` + the agent skill, you don't need a `config.toml`. For the TUI, copy and edit:

```sh
cp config.example.toml ~/.config/helm/config.toml
```

Loaded in order: cwd → platform config dir (`~/.config/helm/` Linux/BSD, `~/Library/Application Support/helm/` macOS) → `$XDG_CONFIG_HOME/helm/`. Helm prints the resolved path to stderr on startup. `config.toml` is gitignored.

Hosts come from `[[hosts]]` entries *and* `~/.ssh/config` Host blocks (wildcards skipped). For ssh-config-discovered hosts that aren't OpenBSD, tag the OS:

```toml
[ssh_config.os]
mac = "macos"
linux-vps = "linux"
```

Every tmux invocation helm makes (remote and local) carries the flags from `tmux_flags`, which defaults to `["-u"]` — forces UTF-8 so unicode renders even when the remote locale doesn't advertise it. Set `tmux_flags = []` to disable, or list your own (`["-u", "-2"]`). This also applies to `helm shell`, which otherwise reads no config.

## SSH

Helm shells out to the system `ssh` binary. Requirements:

- Every `ssh_alias` must be a Host entry in `~/.ssh/config`.
- `ssh-agent` must be loaded — helm has no key-passphrase UI.
- `ProxyJump`, `IdentityFile`, `Port`, etc. live in `~/.ssh/config`, not in helm.

At startup helm runs `ssh-add -l`, fingerprints each `IdentityFile` referenced by your hosts, and **refuses to open the TUI** if a key isn't loaded. The exact `ssh-add` command is printed to stderr.

`helm auth` exposes the same check standalone (exit 0 OK, 1 missing). `helm auth --load` shells out to `ssh-add <path>` for each missing key.

## Layout

```
src/
├── main.rs               event loop, key dispatch
├── app.rs                App state, Mode/RunnerState, event ingestion
├── activity.rs           append-only JSONL audit log
├── config.rs             TOML loader, ssh-config merge
├── ssh/
│   ├── sshconfig.rs      ~/.ssh/config parser
│   ├── agent.rs          ssh-agent fingerprint diff
│   ├── collect.rs        per-OS service / process collectors
│   └── run.rs            spawn ssh -tt, mpsc stream, password-prompt heuristic
├── ipc/                  control socket: server (TUI/daemon) + client (helm exec)
├── tmux.rs               session naming, ensure_session, list, send-keys, capture
├── inventory/            services / processes / ports / health / dns parsers
├── history.rs            SQLite-backed run history
└── ui/                   ratatui draw + per-mode renderers (incl. snapshots)
```

## Testing

```sh
cargo test
cargo clippy --no-deps --all-targets -- -D warnings
```

TUI snapshot tests live under `src/ui/snapshots.rs`. Each renders through `ratatui::backend::TestBackend` and diffs against a fixture. To re-baseline after an intentional UI change:

```sh
HELM_UPDATE_SNAPSHOTS=1 cargo test ui::snapshots
git diff src/ui/snapshots/
```

## License

MIT. See `LICENSE`.
