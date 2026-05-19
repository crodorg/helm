# helm

TUI fleet manager for a small set of OpenBSD VPSs and the businesses inside them.

Single-operator workflow. Built around the assumption that you already have a working `~/.ssh/config`, a loaded `ssh-agent`, and a handful of hosts you log into often. Helm gives you one place to browse them, drop into shells, and run ad-hoc remote commands — including ones that need a `doas` password.

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
- Press `v` → vultr pane: shells out to `curl` against `GET /v2/instances` + `/v2/plans` (set `VULTR_API_KEY`); table shows label / region / plan / $/mo / status / power / IP; Browse detail pane gets a `vultr` line for any host whose `hostname` matches a Vultr `main_ip`
- Press `m` → money pane: shells out to `stripe-pp-cli balance` + `mercury-pp-cli accounts` in parallel (each CLI handles its own auth via `STRIPE_SECRET_KEY` / `MERCURY_BEARER_AUTH`); Stripe block shows available / pending / total, Mercury table lists each account with current + available balance and a row-1 total
- Press `l` → log picker: built-in defaults (messages / daemon / authlog) plus any `[[logs]]` from config that match the selected host; single-char key launches `ssh -tt <alias> tail -n 200 -f <path>` streaming live into a scrolling pane (capped at 5000 lines); Esc kills the tail and returns to Browse
- SQLite history cache at `$XDG_DATA_HOME/helm/state.db` persists every `helm exec` (agent) and Runner (operator) command across helm restarts; AgentTail rehydrates the last 100 agent runs on startup so the transcript survives
- Doas / sudo / ssh-passphrase prompts trigger a centered password modal; submitted password is piped to the remote stdin and never persisted
- `helm shell open <alias>` (CLI subcommand) attaches a terminal to a persistent tmux session on the remote VPS — sessions survive helm restarts and network drops; `helm shell send / read / list / close` drive the same session for scripted or AI-assisted workflows

Not yet wired:
- BuyVM provider API
- DNS record sanity check
- Postmark business overlay

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
- `m` — money pane (needs `stripe-pp-cli` + `mercury-pp-cli` auth)
- `l` — logs picker (built-in defaults + `[[logs]]` from config)
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
│   └── health.rs         curl + openssl x509 parsers + local probe runner
├── vultr.rs              GET /v2/instances + /v2/plans via curl + serde_json
├── money.rs              stripe-pp-cli + mercury-pp-cli shell-out + parsers
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
    └── log_tail.rs       scrolling pane that auto-sticks to the latest line
```

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
