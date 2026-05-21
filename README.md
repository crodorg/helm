# helm

**An AI agent and I share the same tmux pane. Here's the discipline — and the CLI that enforces it.**

![helm demo — sessions pane + detached-ensure](docs/demo.gif)

> Three rules every interaction obeys. The agent never breaks them, and neither do I.

- **Read before send.** Every interaction starts with `helm shell read <alias>` to confirm the pane is at a clean prompt — not mid-command, not inside `vim`, not staring at a password prompt. Blind sends are forbidden.
- **Narrate intent before sending.** Two sentences max, in chat, before any keystrokes land. Gives the human time to interrupt before the agent does anything visible in the shell.
- **Refuse to type passwords.** When `read` shows a `password:` or `passphrase:` line, the agent stops and tells the operator. The human answers in their own attached tmux pane.

That's the whole etiquette. `helm` is one Rust binary that hands an AI agent four primitives — `helm shell open / send / read / list` — for driving a persistent tmux session the operator is already attached to. Local or ssh-remote, same CLI.

## Why this exists

Claude Code's `Bash` tool is one-shot and stateless ([#9881](https://github.com/anthropics/claude-code/issues/9881), [#4319](https://github.com/anthropics/claude-code/issues/4319)). Interactive commands hang it; `cd` doesn't survive; you can't watch a long-running process. The fix is obvious — give the agent a real tmux session — and the community converged on it through 2025 ([TmuxAI](https://github.com/alvinunreal/tmuxai), [mitsuhiko/agent-stuff](https://github.com/mitsuhiko/agent-stuff/blob/main/skills/tmux/SKILL.md), [tmux-mcp](https://github.com/bnomei/tmux-mcp), [Hiren Patel's tag-teaming pattern](https://patelhiren.com/blog/tag-teaming-claude-code-with-ai-agent/)).

What `helm` adds is a **plain CLI binary** (no MCP server registration, no skill registry hop, no raw `tmux send-keys` smell) plus an **encoded discipline** the agent reads as a skill before it touches the shell. Drop the binary on `$PATH`, drop the skill file in front of the agent, attach to the session from your own terminal — you're done.

## Quickstart

```sh
# macOS
brew tap crodorg/helm
brew install helm

# Linux / OpenBSD (cargo build from source)
git clone https://github.com/crodorg/helm
cd helm && cargo build --release
ln -s "$PWD/target/release/helm" ~/.local/bin/helm   # or your bin dir
```

Open a shared tmux session against any host (or your own machine via the reserved `local` alias):

```sh
helm shell open mac              # attach (creates if missing)
helm shell open -d mac:deploy    # ensure exists, stay detached
helm shell list mac              # list helm-* sessions on mac
helm shell read mac              # capture current pane scrollback
helm shell send mac 'uptime'     # type a line + press Enter
```

Hand the agent the skill at [`.claude/skills/helm-shell/SKILL.md`](.claude/skills/helm-shell/SKILL.md). It encodes the three rules above plus the read-then-send loop, label conventions for parallel work, and a `ssh-agent` socket bridge pattern so the assistant's own Bash invocations can ssh out under your loaded keys.

## Prior art

Naming the neighborhood so you know where helm fits:

- **[TmuxAI](https://github.com/alvinunreal/tmuxai)** — closest commercial-flavored sidekick. Watches the operator's pane read-only and runs commands in a dedicated execution pane (different design: separate panes, not shared). Local-only.
- **[mitsuhiko/agent-stuff tmux skill](https://github.com/mitsuhiko/agent-stuff/blob/main/skills/tmux/SKILL.md)** — Armin Ronacher's skill that teaches Claude to `tmux send-keys` directly. Encodes read-before-send via polling. No CLI wrapper, no password-refusal rule, no narrate-before-send.
- **[bnomei/tmux-mcp](https://github.com/bnomei/tmux-mcp)** — full-featured MCP server in Rust, ssh-remote capable via `TMUX_MCP_SSH`. Lives behind MCP server registration in your agent's config rather than as a CLI.
- **[Hiren Patel's "Tag-Teaming Claude Code via Tmux"](https://patelhiren.com/blog/tag-teaming-claude-code-with-ai-agent/)** — blog post documenting the shared-pane handoff pattern with raw `tmux` commands. No tool, no discipline file.
- **Orchestrator class** ([Tmux-Orchestrator](https://github.com/absmartly/Tmux-Orchestrator), [awslabs/cli-agent-orchestrator](https://github.com/awslabs/cli-agent-orchestrator), [claude_code_agent_farm](https://github.com/Dicklesworthstone/claude_code_agent_farm), [amux](https://github.com/mixpeek/amux), [NTM](https://vibecoding.app/blog/ntm-review)) — different problem entirely: spawn N parallel agents each in its own pane while the human watches a dashboard. helm is sidekick, not swarm.

Where helm differs:

| | CLI binary | Skill (encoded) | ssh-remote | Refuses passwords | Sidekick (shared pane) |
|---|---|---|---|---|---|
| **helm** | ✓ | ✓ read+narrate+refuse | ✓ | ✓ | ✓ |
| mitsuhiko/agent-stuff | — (raw tmux) | ✓ read-before-send | indirect | — | ✓ |
| bnomei/tmux-mcp | — (MCP) | — | ✓ | — | ✓ |
| TmuxAI | own binary | — | — | — | separate execute pane |
| Tmux-Orchestrator class | varies | — | spawn-only | — | — (own panes) |

## Beyond the agent surface: helm as a fleet TUI

`helm` (no args) opens a TUI for the wider workflow it was originally built for — managing a small OpenBSD fleet. Browse hosts, run ad-hoc remote commands with a `doas`-prompt-aware password modal, check `rcctl` service health, see your Vultr instance bill, sum Stripe + Mercury balances, tail logs, check DNS, replay past runs from a SQLite history. Press `S` from Browse to land in the **sessions pane** — a live table of every active `helm shell` session across the fleet, with `Enter` to attach.

This part of helm is opinionated for one specific shape of fleet:

- **OpenBSD** on the remote side (uses `rcctl`, `doas`, `acme-client`, `tail -f` — no Linux/systemd/journalctl).
- **`~/.ssh/config` + loaded `ssh-agent`** as the only auth path. Helm never reads private keys.
- **Tens of hosts**, not hundreds. No multi-tenant auth, no RBAC, no audit log.
- The **money pane** shells out to `stripe-pp-cli` + `mercury-pp-cli` from the open-source [printing-press-library](https://github.com/mvanhorn/printing-press-library); helm degrades gracefully if the CLIs are missing.

If that doesn't match your setup, `helm shell` and `helm daemon` work standalone — you can ignore the TUI entirely.

The code is MIT-licensed and small (~6 KLOC).

## The four `helm shell` primitives

What the agent calls. What the human sees. Same session, no duplication.

| Command | What it does |
|---|---|
| `helm shell open <target>` | Attach this terminal to the session. Creates it if missing. The human runs this in their own terminal. |
| `helm shell open -d <target>` | Same, but stays detached. The agent uses this to pre-create a session it intends to drive remotely. |
| `helm shell read <target>` | Capture scrollback from the session's active pane. The agent runs this *before every send*. |
| `helm shell send <target> <text>` | Type the line followed by Enter. The keystrokes land in the same pane the human is attached to. |
| `helm shell list <alias>` | List `helm-*` sessions on that alias's tmux server. |
| `helm shell close <target>` | Kill the session. |

`<target>` is `<alias>` (default session `helm`) or `<alias>:<label>` (session `helm-<label>`). The reserved alias `local` short-circuits ssh and uses the operator's own tmux server — handy for sessions that need interactive `doas` password entry or that should outlive a single ssh connection.

`helm shell` is fundamentally different from `helm exec <alias> <cmd>`, which is one-shot and stateless. `helm shell` retains cwd, env, history, and in-progress prompts across calls; `helm exec` runs and exits.

The skill at [`.claude/skills/helm-shell/SKILL.md`](.claude/skills/helm-shell/SKILL.md) is the canonical agent-facing instruction set: the three discipline rules, the read-then-send loop, label conventions for parallel work, and a `ssh-agent` bridge pattern for sharing the operator's loaded agent socket with the assistant's own Bash subprocess.

## `helm daemon`

`helm exec` connects to a control socket. When the TUI is open the TUI owns the socket; when it isn't, a `helm daemon` does. Either way, an AI agent calling `helm exec <alias> <cmd>` from a separate shell gets the same streamed output, the same SQLite history row, and the same agent-tail entry the operator sees on next launch.

```sh
helm daemon                # foreground; SIGINT / SIGTERM exit cleanly
helm daemon start          # spawn detached; exit once the socket answers
helm daemon stop           # ask a running daemon to exit
helm daemon status         # exit 0 if a daemon (or TUI) is reachable
```

Coexistence is automatic:
- Starting the TUI while a daemon is running quietly shuts the daemon down so the TUI can bind the socket.
- Closing the TUI re-spawns `helm daemon start` so `helm exec` stays reachable. Set `auto_daemon = false` in `config.toml` to opt out.

Only one helm process (TUI or daemon) binds the socket at a time — there is no shared-DB write contention to worry about. The socket lives at `$XDG_RUNTIME_DIR/helm.sock` on Linux (with a fallback chain through `$XDG_CACHE_HOME/helm/helm.sock`), and at `~/Library/Caches/helm/helm.sock` on macOS.

## Audit log — `activity.jsonl`

Every `helm exec` and every `helm shell {open,send,read,list,close}` writes one JSON line to:

- Linux/BSD: `$XDG_STATE_HOME/helm/activity.jsonl` (defaults to `~/.local/state/helm/activity.jsonl`)
- macOS: `~/Library/Application Support/helm/activity.jsonl`

The TUI's **agent activity** pane (`c` from Browse) renders this file as a scrollable list of rows: time, exit status, kind (`exec` / `send` / `read` / `open` / `close` / `list`), target (`alias:label`), command, and a 1-line output preview. Privilege-escalating commands (any `doas` / `sudo` token at the start of a command or after `|` / `&&` / `;`) are tagged with a red `[DOAS]` badge.

The log is append-only and agent-agnostic — it doesn't matter whether Claude Code, Cursor, Aider, or a bash one-liner invoked the CLI; the same record gets written. You can `tail -f` the file from any other terminal:

```sh
tail -f ~/.local/state/helm/activity.jsonl | jq .
```

So even with the TUI closed, you have a real-time feed of what any agent is doing.

## `helm auth`

Standalone CLI subcommand for non-TUI use. Reads `config.toml` + `~/.ssh/config`, recomputes the IdentityFile fingerprints helm hosts depend on, and exits with a status that reflects ssh-agent state:

```sh
helm auth              # exit 0: agent OK; 1: missing/unreachable; 2: arg error
helm auth --load       # if keys missing, exec `ssh-add <path>` per key (prompts
                       # for the passphrase), then re-check
helm auth help         # usage
```

Wire into login shells or doas wrappers — for example:

```sh
# ~/.kshrc — load the VPS key on first interactive shell
helm auth --load >/dev/null 2>&1 || echo "helm: vps keys not loaded"
```

## Configuration

If you only ever use `helm shell` + the agent skill, you don't need a `config.toml` at all. For the TUI fleet manager surface, copy and edit:

```sh
cp config.example.toml ~/.config/helm/config.toml
$EDITOR ~/.config/helm/config.toml
```

`config.toml` is loaded from, in order:
1. the current working directory,
2. the platform-native config dir (`~/.config/helm/config.toml` on Linux/OpenBSD, `~/Library/Application Support/helm/config.toml` on macOS),
3. and — for macOS users who keep a single cross-machine config — `~/.config/helm/config.toml` (or `$XDG_CONFIG_HOME/helm/config.toml`).

Helm prints the chosen path to stderr on startup so you can confirm which file actually got loaded. `config.toml` is gitignored.

The OpenBSD log defaults (`/var/log/messages`, `daemon`, `authlog`) only make sense against OpenBSD hosts; macOS users tailing logs on their own machine via the `local` alias should add explicit `[[logs]]` entries pointing at `/var/log/system.log` or whatever they actually want.

## SSH expectations

Helm shells out to the system `ssh` binary for everything. That means:
- `ssh_alias` in `config.toml` must be a Host entry in `~/.ssh/config`
- `ssh-agent` must be loaded — helm has no key-passphrase UI
- `ProxyJump`, `IdentityFile`, `Port`, etc. live in `~/.ssh/config`, not in helm

Helm also reads `~/.ssh/config` directly at startup. Every named Host block (wildcards and IP-literal aliases skipped) becomes a candidate host. A matching TOML entry — same `ssh_alias` — wins on `name`/`provider`/`notes`; ssh config backfills `hostname`/`user` when the TOML entry omits them. Ssh-only aliases show up with provider `?` (or `LOCAL` for RFC1918 / loopback). Suppress noisy aliases via `[ssh_config] ignore = ["..."]`. Disable the whole feature with `[ssh_config] enabled = false`.

Synthesized hosts default to `os = "openbsd"`. If an ssh-config-discovered alias is a mac or systemd Linux box, tag it so the Services pane (`s`) dispatches to the right init system instead of running `rcctl ls`:

```toml
[ssh_config.os]
mac = "macos"
linux-vps = "linux"
```

An explicit `[[hosts]].os` always wins over the override map.

### Agent check

At startup, helm runs `ssh-add -l`, computes the fingerprint of each `IdentityFile` referenced by your hosts (via the matching `.pub` file and `ssh-keygen -lf`), and **refuses to open the TUI** if any expected key isn't loaded or if ssh-agent isn't reachable. The exact `ssh-add` command(s) to run are printed to stderr, so you can copy-paste, load the key, then re-run helm. Typical workflow with a passphrased VPS key shared across hosts:

```sh
ssh-add ~/.ssh/id_ed25519_vps   # once per session, enter passphrase
helm                            # all VPS aliases auth via agent, no per-host prompts
```

If `.pub` is missing or `ssh-keygen` can't read it, helm silently skips that file (no false alarm). Helm never reads the private key itself; passphrase entry stays out of the TUI by design.

The `r` runner spawns `ssh -tt <alias> <cmd>` so the remote allocates a PTY. This is what lets `doas` write its prompt to a stream helm can see.

## Keys

Browse:
- `j` / `k` — move
- `Enter` — drop to interactive ssh on selected host
- `r` — open runner
- `s` — services pane
- `S` — sessions pane (lists every live `helm shell` tmux session across all hosts + `local`; Enter attaches, `d` ensures detached)
- `p` — processes pane
- `H` — health pane (capital — lowercase `h` is "back" in every other mode, vim-style)
- `v` — vultr pane (needs `VULTR_API_KEY`)
- `m` — money pane (needs `stripe-pp-cli` + `mercury-pp-cli` auth)
- `l` — logs picker (built-in defaults + `[[logs]]` from config)
- `t` — history pane (past `helm exec` + Runner runs from `state.db`; Enter replays into the runner)
- `d` — dns pane (per-business A/AAAA/MX/CAA, verdict vs the host's IP)
- `a` — shortcuts palette
- `c` — agent tail
- `?` — in-TUI help (key list for the current pane; works from any non-text-input mode)
- `R` — refresh-all overlays (re-fires vultr + money + postmark + dns + health in one shot)
- `F5` — reload `config.toml` (re-merges ssh_config; clamps selected host if list shrank)
- `q` / `Esc` — quit

Runner (typing command):
- `Enter` — run on selected host
- `Esc` — back to browse

Runner (running / password modal):
- type password, `Enter` — submit
- `Esc` — cancel password input

Runner (idle, after a command finished):
- `r` — new command
- `Esc` — back to browse

## Layout

```
src/
├── main.rs               event loop, terminal setup, key dispatch
├── app.rs                App state, Mode/RunnerState, event ingestion
├── config.rs             TOML loader, Host / Business / Provider, ssh-config merge
├── ssh/
│   ├── mod.rs
│   ├── sshconfig.rs      ~/.ssh/config parser → Vec<SshHost>
│   ├── agent.rs          ssh-agent fingerprint diff + render_blocker
│   ├── collect.rs        fire-and-collect remote command helper (rcctl triple)
│   └── run.rs            spawn ssh -tt, mpsc stream, password-prompt heuristic
├── inventory/
│   ├── services.rs       rcctl ls parser
│   ├── processes.rs      ps parser
│   ├── ports.rs          netstat parser
│   ├── health.rs         curl + openssl x509 parsers + local probe runner
│   └── dns.rs            drill/dig wrapper + A-vs-expected-IP verdict
├── vultr.rs              GET /v2/instances + /v2/plans via curl + serde_json
├── money.rs              stripe-pp-cli + mercury-pp-cli shell-out + parsers
├── postmark.rs           curl + Postmark /stats/outbound parser (token via stdin)
├── history.rs            rusqlite-bundled HistoryStore: runs + run_lines tables
└── ui/
    ├── mod.rs            mode router, header, footer
    ├── browse.rs         host list + detail (incl. vultr line when matched)
    ├── runner.rs         output stream + command input + password modal
    ├── services.rs       service-state table
    ├── processes.rs      processes + listening sockets table
    ├── health.rs         per-business HTTP + TLS table
    ├── vultr.rs          per-instance table from VultrCache
    ├── money.rs          Stripe block + Mercury accounts table
    ├── log_picker.rs     modal palette: keyed shortcuts → tail paths
    ├── log_tail.rs       scrolling pane that auto-sticks to the latest line
    ├── history.rs        run-history table (replay on Enter)
    └── dns.rs            per-business A/AAAA/MX/CAA table
```

## Money pane / pp CLIs

The money pane (`m`) does not link directly to Stripe or Mercury — it shells out to two CLIs and parses their JSON. The CLIs are `stripe-pp-cli` and `mercury-pp-cli`, both from the open-source [printing-press-library](https://github.com/mvanhorn/printing-press-library). Install them once:

```sh
go install github.com/mvanhorn/printing-press-library/library/payments/stripe/cmd/stripe-pp-cli@latest
go install github.com/mvanhorn/printing-press-library/library/payments/mercury/cmd/mercury-pp-cli@latest
```

Make sure `$GOPATH/bin` (or `$GOBIN`) is on `$PATH`. If the CLIs are absent the pane gracefully degrades to a "(CLI not found)" message — the rest of helm keeps working.

You can also bring your own balance source: helm just expects a binary on `$PATH` with the matching name that prints the JSON shape below.

`stripe-pp-cli balance` → matches the [Stripe `/v1/balance` response](https://docs.stripe.com/api/balance):

```json
{
  "object": "balance",
  "available": [{"amount": 12345, "currency": "usd"}],
  "pending":   [{"amount": 678,   "currency": "usd"}]
}
```

`mercury-pp-cli accounts` → the [Mercury `/api/v1/accounts` shape](https://docs.mercury.com/reference/get-all-accounts) trimmed to:

```json
{ "accounts": [
    { "id": "...", "name": "Operating", "kind": "checking",
      "currentBalance": 12345.67, "availableBalance": 12000.00,
      "currency": "USD" }
]}
```

Either CLI is responsible for its own auth — helm passes nothing through. The printing-press CLIs honor `STRIPE_SECRET_KEY` and `MERCURY_BEARER_AUTH` env vars (see their READMEs); a one-off wrapper around `curl` + Stripe's REST API will work just as well.

## Services pane / per-host OS family

The Services pane (`s` from Browse) is the only inventory pane that diverges across operating systems. Pick the right init system per host in `config.toml`:

```toml
[[hosts]]
name = "web"
ssh_alias = "web"
os = "openbsd"     # openbsd | debian | macos — defaults to openbsd
```

What each value runs:

| `os`      | Init system | Command helm fires                                                    |
|-----------|-------------|------------------------------------------------------------------------|
| `openbsd` | `rcctl`     | three parallel `doas -n rcctl ls {on,started,failed}` calls (see below)|
| `linux`   | `systemctl` | `systemctl list-units --type=service --all --no-legend --plain --no-pager` |
| `macos`   | `launchctl` | `launchctl list` (user-domain services only)                          |

`linux` covers **any systemd-based distro** — Debian, Ubuntu, RHEL, Arch, Fedora, openSUSE, Devuan-with-systemd, etc. The driver is systemd, not the distro. Non-systemd Linux (Void / runit, Alpine / OpenRC, Gentoo / s6) isn't recognized yet — open an issue if you need it.

Debian + macOS calls run unprivileged — no `sudo` / `doas` prefix — because listing is read-only. **OpenBSD's `rcctl` needs root**, however, because `rcctl ls started|failed` calls `_rc_check` per service and some pidfiles are root-owned (postgres, openresolvd, etc.). Add three lines to `/etc/doas.conf` on each OpenBSD host so the call doesn't hang on a password prompt the pane can't answer:

```
permit nopass <user> cmd rcctl args ls on
permit nopass <user> cmd rcctl args ls started
permit nopass <user> cmd rcctl args ls failed
```

Replace `<user>` with the ssh user from your `~/.ssh/config` Host block.

The `os` field falls back to `openbsd` for backwards compatibility — helm grew up on an OpenBSD fleet. Linux and macOS hosts must set it explicitly or the Services pane will fire `rcctl` on them and get nothing.

## Optional panes (`[features]`)

The Browse pane ships several side panes that are only useful when their backing dependency exists. **All default to off** so a fresh install shows only the panes everyone uses. Opt in by flipping the relevant flag in `config.toml`:

```toml
[features]
health = false   # H — HTTPS reachability + TLS expiry per business
vultr  = false   # v — Vultr instance overlay (needs $VULTR_API_KEY)
dns    = false   # d — per-business A / AAAA / MX / CAA table
money  = false   # m — Stripe + Mercury balances (needs pp CLIs)
```

Disabled panes are hidden from the Browse keys palette **and** the help overlay, so the UI looks like those panes don't exist until you turn them on. The dispatch handler treats the key as a no-op too — pressing `m` with `money = false` does nothing.

Always-on panes (services `s`, sessions `S`, processes `p`, logs `l`, history `t`, shortcuts `a`, agent activity `c`, runner `r`) need no flag — they work against any host with ssh + tmux + (for services) the right init system.

### What's a "business" in helm?

Three of the optional panes (`health`, `dns`, `money`) iterate `[[businesses]]` entries in `config.toml`. "Business" is just helm's noun for **any named thing with a domain** — personal site, side project, brochureware, OSS landing page, your blog, a forum you run for fun. Nothing in the code requires money, employees, or a tax ID. The naming is historical (helm grew up managing a few revenue-generating sites).

Minimum entry for the health pane (`H`) is:

```toml
[[businesses]]
name = "my-blog"
primary_domain = "blog.example.com"
host = "personal"          # an ssh_alias from your [[hosts]] list
```

That's it — no Stripe key, no Mercury account, no Postmark token. With just those three lines the `H` pane gives you HTTPS reachability + days-until-cert-expiry for `blog.example.com`. Add `stripe_account_id` / `mercury_account_id` / `postmark_server_token` only if you also want the money or Postmark overlays.

## Operator-specific bits to know about

A handful of integrations exist because the author wired them in for his own fleet. They degrade cleanly when their backing CLI / API key is missing — the rest of helm keeps working — but if you want them lit up:

- **Money pane (`m`)** — shells out to `stripe-pp-cli` + `mercury-pp-cli` from the open-source [printing-press-library](https://github.com/mvanhorn/printing-press-library). See the dedicated section above. Skip the pane (or just don't open it) if you don't care about per-business balances.
- **Postmark stats** — per-business field `postmark_server_token` in `config.toml` fires a `curl` against Postmark's `/stats/outbound` on startup and renders Sent / Bounced / Spam under the business in Browse. Leave the field unset to skip.
- **Vultr pane (`v`)** — needs `VULTR_API_KEY` in the environment. Without it the pane shows `(set $VULTR_API_KEY to enable)` and the Browse detail panel omits Vultr-derived lines. No Vultr account? Ignore the pane entirely.
- **Log defaults** — the built-in `l` palette varies by the selected host's `os` field. OpenBSD: `m=/var/log/messages`, `d=/var/log/daemon`, `a=/var/log/authlog`. Debian: `s=/var/log/syslog`, `a=/var/log/auth.log`, `k=/var/log/kern.log`. macOS: `s=/var/log/system.log`, `i=/var/log/install.log`, `w=/var/log/wifi.log`. Add `[[logs]]` entries in `config.toml` for app-specific files.
- **Provider enum (`local | vultr | buyvm | unknown`)** — drives the colored tag in Browse and (for `vultr`) which API overlay fires. `buyvm` is a label-only tag (Stallion's REST API was retired, so no overlay); use `unknown` for any provider helm doesn't recognize — it just labels the row.

None of these gate the agent-facing surface. The `helm shell` primitives + sessions pane + daemon + audit log all work on a host with zero overlays configured.

## Testing

```sh
cargo test
cargo clippy --no-deps --all-targets -- -D warnings
```

TUI snapshot tests live under `src/ui/snapshots.rs`. Each test renders a pane through `ratatui::backend::TestBackend` and diffs the cell grid against a fixture in `src/ui/snapshots/*.txt`. When a UI change is intentional:

```sh
HELM_UPDATE_SNAPSHOTS=1 cargo test ui::snapshots
git diff src/ui/snapshots/    # review re-baselined fixtures before committing
```

## License

MIT. See `LICENSE`.
