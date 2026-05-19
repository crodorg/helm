# helm

TUI fleet manager for a small set of OpenBSD VPSs and the businesses inside them.

Single-operator workflow. Built around the assumption that you already have a working `~/.ssh/config`, a loaded `ssh-agent`, and a handful of hosts you log into often. Helm gives you one place to browse them, drop into shells, and run ad-hoc remote commands — including ones that need a `doas` password.

## Why this exists

Tiny fleets — five or ten VPSs, one operator, no Kubernetes, no Datadog — live in an awkward gap. They are too small to justify a control plane and too many to babysit with `ssh + watch + tmux` alone. Helm is the missing inner-loop tool for that gap:

- **One pane for "what's actually running":** `s` shows every `rcctl` service across a host, color-coded by state. `p` shows top CPU + every listening socket. `h` pings each business's primary domain for HTTP status, latency, and TLS expiry — all in parallel, all from your laptop.
- **One pane for "how much is this costing me":** `v` joins Vultr's `/v2/instances` and `/v2/plans` into one table — region, plan, monthly cost, power state, IP. `m` sums Stripe + Mercury balances so you can see runway and revenue without opening two dashboards.
- **One pane for "ssh in and fix it":** `Enter` drops the TUI and hands the terminal to plain `ssh <alias>`. `r` runs ad-hoc commands with a live `doas`-prompt-aware password modal. `l` tails any log file the host knows about. Everything writes through your existing `~/.ssh/config` and `ssh-agent` — no new auth surface.
- **Persistent shells the operator and an AI agent can share:** `helm shell` creates a remote-side tmux session that survives helm restarts, network drops, and laptop sleeps; an attached AI assistant can `read` scrollback and `send` keystrokes while the operator watches live. See [Driving helm from an AI agent](#driving-helm-from-an-ai-agent).

## Is this for you?

Probably not, and that's fine — this is a personal tool built for one specific workflow. Helm assumes:

- **OpenBSD** on the remote side (uses `rcctl`, `doas`, `acme-client`, `tail -f` — no Linux/systemd/journalctl).
- **`~/.ssh/config` + loaded `ssh-agent`** as the only auth path. Helm never reads private keys and has no passphrase UI.
- A small **fleet sized for one human's mental cache** — tens of hosts, not hundreds. No multi-tenant auth, no RBAC, no audit log.
- The **money pane talks to two CLIs (`stripe-pp-cli`, `mercury-pp-cli`) from the open-source [printing-press-library](https://github.com/mvanhorn/printing-press-library)** — install them with `go install` (see [Money pane](#money-pane--pp-clis)). The rest of helm works without them; the pane gracefully degrades when the CLIs are missing.

The code is permissive-licensed (MIT) and small (~5 KLOC). If you run an OpenBSD fleet of similar shape, fork it; if you're here to read how a single-binary Rust TUI ties ssh + tmux + curl + sqlite together, the source is the docs.

## Status

v0.1 ships:
- TOML-driven host + business inventory
- Auto-discovery from `~/.ssh/config` — any Host block is merged into the host list; TOML entries override metadata, ssh config supplies hostname/user
- Browse pane: host list with provider badges, detail with businesses-on-host
- Press `Enter` → suspends TUI, drops to `ssh <alias>`, restores on exit
- Press `r` → runner mode: type a command, stream stdout/stderr live
- Press `s` → services pane: fires three parallel `doas -n rcctl ls {on,started,failed}`, merges into a sorted table (Failed first, then Untracked, Started, Stopped) — requires nopass entries in `/etc/doas.conf` for `rcctl ls` (see Services notes below)
- Press `p` → processes pane: fires `ps -axo` + `netstat -na` in parallel, renders top 20 processes by CPU and all listening sockets
- Press `h` → health pane: per business, runs local `curl` (HTTP status + ms) and `openssl s_client | openssl x509` (TLS expiry) against `primary_domain`; rows fill in as probes return, colored red <14d / yellow <30d / green otherwise
- Press `v` → vultr pane: shells out to `curl` against `GET /v2/instances` + `/v2/plans` (set `VULTR_API_KEY`); table shows label / region / plan / $/mo / status / power / IP; Browse detail pane gets a `vultr` line for any host whose `hostname` matches a Vultr `main_ip`. j/k selects a row; `R` / `H` / `S` / `N` requests reboot / halt / start / snapshot — a confirm modal asks y/n before the POST fires, and the pane auto-refreshes after a 2xx response. Snapshot is billable (~$0.05/GB/mo until you delete it from manage.vultr.com); the confirm modal carries an explicit `BILLABLE` warning to keep that out of the footgun zone
- Press `b` → buyvm pane: shells `curl` against `GET {BUYVM_API_BASE}/services` (default base `https://manage.frantech.ca/api/client`, override with `BUYVM_API_BASE`; set `BUYVM_API_KEY`); table shows label / location / package / $/mo / status / IP; parser is tolerant of both `{"data": [...]}` Stallion-wrapped and bare-array legacy responses, and accepts a handful of field aliases (`hostname`/`primary_ipv4`/`product_name`/etc.); Browse detail gets a `buyvm` line for matched hosts
- Press `m` → money pane: shells out to `stripe-pp-cli balance` + `mercury-pp-cli accounts` in parallel (each CLI handles its own auth via `STRIPE_SECRET_KEY` / `MERCURY_BEARER_AUTH`); Stripe block shows available / pending / total, Mercury table lists each account with current + available balance and a row-1 total. Each `[[businesses]]` may set `stripe_account_id` + `mercury_account_id` — the matching slice renders inline under the business bullet on the Browse detail panel, and the money fetch fires eagerly on startup when any linkage exists
- Postmark stats overlay: `[[businesses]]` may set `postmark_server_token` — helm fires `curl` against `https://api.postmarkapp.com/stats/outbound` (last 30 days UTC) on startup; token rides in `X-Postmark-Server-Token` via curl's `-H @-` stdin so it stays out of argv. Sent / bounced (+ rate) / spam (+ rate) render inline on the Browse detail panel
- Press `l` → log picker: built-in defaults (messages / daemon / authlog) plus any `[[logs]]` from config that match the selected host; single-char key launches `ssh -tt <alias> tail -n 200 -f <path>` streaming live into a scrolling pane (capped at 5000 lines); Esc kills the tail and returns to Browse
- Press `t` → history pane: most-recent 200 runs from `state.db` (agent + operator combined) in a scrolling table — relative time, source, alias, exit code, duration, command; `j/k` to move, Enter to load the selected command back into the runner against the original host for one-key replay/edit
- Press `d` → dns pane: per business, shells `drill -Q <domain> {A,AAAA,MX,CAA}` (prefers `drill`, falls back to `dig +short`); table shows VERDICT (MATCH / MISMATCH / ? / ERROR) based on whether the A set contains the host's `hostname` (when that hostname is an IP literal), plus full AAAA/MX/CAA detail lines underneath
- SQLite history cache at `$XDG_DATA_HOME/helm/state.db` persists every `helm exec` (agent) and Runner (operator) command across helm restarts; AgentTail rehydrates the last 100 agent runs on startup so the transcript survives
- Doas / sudo / ssh-passphrase prompts trigger a centered password modal; submitted password is piped to the remote stdin and never persisted
- `helm shell open <alias>` (CLI subcommand) attaches a terminal to a persistent tmux session on the remote VPS — sessions survive helm restarts and network drops; `helm shell send / read / list / close` drive the same session for scripted or AI-assisted workflows

Not yet wired:
- `helm auth` bootstrap subcommand

## Setup

```sh
cargo build --release
cp config.example.toml config.toml
$EDITOR config.toml
ssh-add ~/.ssh/id_ed25519        # if not already loaded
./target/release/helm
```

`config.toml` is loaded from the current working directory first, then `$XDG_CONFIG_HOME/helm/config.toml`. It is gitignored.

## SSH expectations

Helm shells out to the system `ssh` binary for everything. That means:
- `ssh_alias` in `config.toml` must be a Host entry in `~/.ssh/config`
- `ssh-agent` must be loaded — helm has no key-passphrase UI
- `ProxyJump`, `IdentityFile`, `Port`, etc. live in `~/.ssh/config`, not in helm

Helm also reads `~/.ssh/config` directly at startup. Every named Host block (wildcards and IP-literal aliases skipped) becomes a candidate host. A matching TOML entry — same `ssh_alias` — wins on `name`/`provider`/`notes`; ssh config backfills `hostname`/`user` when the TOML entry omits them. Ssh-only aliases show up with provider `?` (or `LOCAL` for RFC1918 / loopback). Suppress noisy aliases via `[ssh_config] ignore = ["..."]`. Disable the whole feature with `[ssh_config] enabled = false`.

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
- `p` — processes pane
- `h` — health pane
- `v` — vultr pane (needs `VULTR_API_KEY`)
- `b` — buyvm pane (needs `BUYVM_API_KEY`; override base via `BUYVM_API_BASE`)
- `m` — money pane (needs `stripe-pp-cli` + `mercury-pp-cli` auth)
- `l` — logs picker (built-in defaults + `[[logs]]` from config)
- `t` — history pane (past `helm exec` + Runner runs from `state.db`; Enter replays into the runner)
- `d` — dns pane (per-business A/AAAA/MX/CAA, verdict vs the host's IP)
- `a` — shortcuts palette
- `c` — agent tail
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
├── buyvm.rs              GET {BUYVM_API_BASE}/services via curl + serde_json
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
    ├── buyvm.rs          per-service table from BuyvmCache (Stallion API)
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

## Driving helm from an AI agent

`helm shell` creates a tmux session on the remote VPS that the operator attaches to in their own terminal. An AI agent (Claude Code, etc.) drives the same session from the side: `helm shell read <alias>` scrapes scrollback, `helm shell send <alias> "<cmd>"` types a line. The operator sees every keystroke land in real time and can intervene at any point — passwords, Ctrl-C, abort.

This is fundamentally different from `helm exec <alias> <cmd>`, which is one-shot and stateless; `helm shell` retains cwd, env, history, and in-progress prompts across calls.

A ready-to-load Claude Code skill is included at [`.claude/skills/helm-shell/SKILL.md`](.claude/skills/helm-shell/SKILL.md). It is agent-agnostic — drop the same prompt rules into any tool-using LLM. The discipline it encodes:

- **Read before send.** Every interaction starts with `helm shell read <alias>` to confirm the pane is at a clean prompt (not mid-command, not in `vim`, not at a password prompt). Blind sends are forbidden.
- **One logical command at a time.** Send, then read again to verify exit / next prompt before sending more. `helm shell send` only confirms the keystrokes were delivered to tmux — it does not wait for the remote command to finish.
- **Narrate intent in conversation before sending.** Two sentences max. Gives the operator time to interrupt before keys land.
- **Refuse to type passwords.** When `read` shows a `password:` or `passphrase:` line, the agent stops and tells the operator. The operator answers in their attached tmux pane.
- **Use labels for parallel work.** `<alias>:deploy`, `<alias>:logs` — each label is a separate remote tmux session the operator can attach to in its own window.

The skill file documents the four primitives (`open -d`, `read`, `send`, `list`), an `ssh-agent` bridge pattern for sharing the agent socket between operator and assistant Bash invocations, and the read-then-send workflow in detail. Open it for the full guide.

## Services pane / doas

`rcctl ls started|failed` walks every rc.d service and calls `_rc_check` on each; for services whose pidfile is root-owned (postgres, openresolvd, etc.), `_rc_check` needs root. Helm therefore prefixes `doas -n` so the command fails fast with an explicit authorization error rather than hanging on a password prompt the pane can't answer. Add three lines to `/etc/doas.conf` on each target host:

```
permit nopass <user> cmd rcctl args ls on
permit nopass <user> cmd rcctl args ls started
permit nopass <user> cmd rcctl args ls failed
```

Replace `<user>` with the ssh user from your `~/.ssh/config` Host block.

## Testing

```sh
cargo test
cargo clippy --no-deps --all-targets -- -D warnings
```

## Roadmap

In rough order:

1. ~~Services pane (`s` from browse)~~ — done
2. ~~Process / port inventory (`ps`, `netstat`)~~ — done
3. ~~TLS expiry + HTTP healthcheck for each business~~ — done
4. ~~Vultr API integration — augment host list with region/cost/state~~ — done
5. ~~Stripe + Mercury overlay (shell out to `stripe-pp-cli`, `mercury-pp-cli`)~~ — done
6. ~~Logs tail viewer (`l` from browse, built-in + config-driven `tail -f`)~~ — done
7. ~~SQLite history cache (persists agent + operator runs; rehydrates AgentTail on startup)~~ — done
8. ~~Runner history pane — keybind that opens a list of past runs from `state.db`, sorted by host/recency, with one-key replay~~ — done
9. ~~Per-business Stripe + Mercury linkage — map each `[[businesses]]` to one Stripe account + one Mercury account; render its slice on the business detail panel instead of one fleet-wide block~~ — done (Mercury renders per-account balance inline; Stripe shows a linkage badge — per-Connect-account balance is a follow-up)
10. ~~DNS sanity check — for each business `primary_domain`, resolve A/AAAA + MX + CAA and surface mismatches against the host's known public IP~~ — done
11. ~~Postmark stats overlay — per-business send / bounce / spam-complaint counts via Postmark's stats API~~ — done
12. ~~BuyVM Stallion panel — same shape as the Vultr pane but against BuyVM's Stallion API~~ — done
13. ~~Vultr actions — reboot / stop / start / snapshot from the Vultr pane (with a confirm modal — these are irreversible)~~ — done
14. `helm auth` subcommand — one-shot bootstrap that loads the VPS key into `ssh-agent`, verifies fingerprints across all hosts, and exits 0 / non-zero so it can be wired into login shells or doas wrappers
