# Looper — Autonomous AI Dev Team for Your GitHub Repos

[![CI](https://img.shields.io/github/actions/workflow/status/quangdang46/looper/ci.yml?branch=main&logo=github&label=CI)](https://github.com/quangdang46/looper/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-stable-blue?logo=rust)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![GitHub release](https://img.shields.io/github/v/release/quangdang46/looper?logo=github)](https://github.com/quangdang46/looper/releases)
[![LoC](https://img.shields.io/badge/Rust-37K%20LOC-orange)](#)
[![Go legacy](https://img.shields.io/badge/Go%20legacy-166K%20LOC%20%E2%86%92%20-78%25-lightgrey)](#)

**Looper** monitors GitHub repositories, and when an issue is labeled `looper:plan`, it coordinates a team of AI agents to implement the feature end-to-end: planning, reviewing, fixing, and iterating until every check passes.

Unlike CI/CD pipelines that only veto broken code, Looper *writes* and *fixes* code. It is a persistent daemon that lives alongside your repos, handling the grunt work of feature implementation so you can focus on architecture and decisions.

Built in Rust (`tokio` async). Ported from a Go original (166K LOC -> 37K LOC — 78% smaller).

---

## Quick Start

### Install with one command

```bash
curl -fsSL https://github.com/quangdang46/looper/releases/latest/download/install.sh | bash
```

This installs three binaries to `~/.local/bin/`:

| Binary | Role |
|--------|------|
| `looper` | CLI client (talks to the daemon REST API) |
| `looperd` | Daemon (long-running background process) |
| `loopernet` | Cloud coordination server (multi-node mode) |

### Or with Homebrew

```bash
# Coming soon — tap not yet published
# brew install quangdang46/tap/looper
```

### Manual build

```bash
git clone https://github.com/quangdang46/looper.git
cd looper
cargo build --release
# Binaries in target/release/{looper,looperd,loopernet}
```

### Start the daemon

```bash
# Start in foreground (default)
looperd

# Or start as a background service
looper daemon install
looper daemon start
```

The daemon opens a REST API on `http://127.0.0.1:7391`.

---

## Architecture

Looper is a Cargo workspace of 16 crates organized in a dependency chain, topped by three binaries:

```
                    ┌──────────────────────┐
                    │     GitHub Issues     │
                    │  (labeled looper:plan)│
                    └──────────┬───────────┘
                               │ webhook / poll
                               ▼
┌─────────────────────────────────────────────────────┐
│                   looperd (daemon)                   │
│                                                      │
│  ┌──────────┐  ┌──────────┐  ┌──────────────────┐   │
│  │ Scheduler │─▶│  Runner  │──▶  Agent Executor  │   │
│  │ (tick)    │  │  (5 roles)│  │  (5 vendors)    │   │
│  └──────────┘  └──────────┘  └──────────────────┘   │
│         │            │                │              │
│         ▼            ▼                ▼              │
│  ┌──────────┐  ┌──────────┐  ┌──────────────────┐   │
│  │ Storage   │  │ Service  │  │ GitHub / Git     │   │
│  │ (SQLite)  │  │ (logic)  │  │ Gateway          │   │
│  └──────────┘  └──────────┘  └──────────────────┘   │
│         ▲                                            │
│  ┌──────┴──────┐                                     │
│  │  REST API   │  port 7391                          │
│  │  + SSE      │                                     │
│  └─────────────┘                                     │
└─────────────────────────────────────────────────────┘
         │                              │
         ▼                              ▼
┌─────────────────┐          ┌─────────────────────┐
│  looper (CLI)   │          │  loopernet (cloud)  │
│  health / projects│         │  multi-node coord.  │
│  loops / queue    │          │  claim routing      │
└─────────────────┘          └─────────────────────┘
```

### Crate dependency graph

```
looper-types           (domain types, state machines, zero deps)
  └── looper-config    (config loading, validation, disclosure)
  └── looper-storage   (SQLite repositories, migrations, event log)
        └── looper-service  (loop/run/project business logic)
              ├── looper-github   (GitHub gateway via gh CLI)
              ├── looper-git      (git worktree management via git2)
              ├── looper-agent    (agent executor, 5 vendors)
              ├── looper-scheduler (tick loop, queue, claim)
              └── looper-runner   (5 agent roles, ~20K lines)
                    ├── looper-api       (axum REST server, auth, SSE)
                    ├── looper-webhook   (webhook forwarding)
                    └── looper-infra     (bootstrap, runtime, notif)
                          ├── looperd    (daemon binary)
                          ├── looper-cli (CLI binary)
                          └── looper-net (cloud server binary)
```

---

## How It Works

### 1. Issue Detection

The daemon polls GitHub or receives webhooks. When an issue is labeled `looper:plan`, the loop begins.

### 2. Agent Loop

Looper runs a multi-agent workflow on every accepted issue:

```
┌─────────┐    ┌──────────┐    ┌────────┐    ┌────────────┐
│ Plan    │───▶│ Review   │───▶│ Work   │───▶│ Review     │
│ Agent   │    │ Agent    │    │ Agent  │    │ / Fix      │
└─────────┘    └──────────┘    └────────┘    └────────────┘
     │              │              │               │
     ▼              ▼              ▼               ▼
  Writes        Checks        Implements       Iterates
  a spec        the plan      the code         until green
```

Each step runs a **disposable agent process** — the agent is invoked, given a prompt context, and its output is captured and committed. If the review fails, a **fixer** agent patches the PR and the loop repeats.

### 3. Agent Vendors

Looper supports 5 agent backends, selected per run or globally:

| Vendor | Identifier | Notes |
|--------|-----------|-------|
| Claude Code | `claude-code` | Anthropic's official CLI |
| Codex CLI | `codex` | OpenAI Codex CLI |
| OpenCode | `opencode` | Open-source agent CLI |
| Cursor CLI | `cursor` | Cursor editor CLI mode |
| Custom | `custom` | Any executable conforming to the agent protocol |

### 4. Priority Queues

Issues and PRs are slotted into three queues:

| Queue | Type | Description |
|-------|------|-------------|
| Auto | From config | Auto-discovered repos matched by label/author |
| Planned | `looper:plan` label | Issues explicitly tagged for Looper |
| Manual | CLI / API | Ad-hoc queue items via `looper queue enqueue` (scheduler-driven) |

### 5. Persistence

All state is stored in a local SQLite database: queue entries, run history, agent outputs, event logs, and configuration. The daemon is stateless enough to restart safely — interrupted runs can be resumed.

---

## CLI Usage

The `looper` CLI is a REST client for `looperd`. Default daemon URL:

```text
http://127.0.0.1:7391
```

Override with global `--daemon-url` (or auth with `--token`). Most commands also accept `--json` for machine-readable output.

> **Honesty note:** Only the commands below are on the primary surface (`looper --help`). Hidden/stub subcommands (if any) exit non-zero as unsupported — do not treat them as working. There is no Go-era `looper status`, `looper run <issue-url>`, `looper network *`, `looper webhook *`, `looper inspect`, or `looper upgrade` command.

### Global options

| Flag | Meaning |
|------|---------|
| `--daemon-url <URL>` | Daemon API base (default `http://127.0.0.1:7391`) |
| `--token <TOKEN>` | Bearer token for daemon auth |
| `--json` | JSON output |
| `--no-auto-upgrade` | Skip startup auto-upgrade check |

### Daemon lifecycle

```bash
looper daemon start                    # Start looperd in the background
looper daemon stop                     # Stop the daemon
looper daemon restart
looper daemon status                   # Is the process running?
looper daemon logs [N]                 # Tail recent log lines
looper daemon install                  # launchd (macOS) / systemd (Linux)
looper daemon uninstall

# Or run the binary in the foreground:
looperd
```

### Health & version

```bash
looper health                          # GET /health — must work against default 7391
looper health --json
looper version
looper shutdown                        # Ask daemon to shut down via API
looper reload                          # Reload daemon config via API
```

### Projects (GitHub repo binding required)

Work discovery and GitHub gateway calls need a **resolvable GitHub repo** (`owner/name`) on the project. Set it with `--repo-url`, or pass a local `--path` whose `remote.origin.url` is a github.com remote so the daemon can auto-detect `metadata.repo`. Without that binding, admit-work / discovery fail closed.

```bash
looper projects add myapp \
  --path /path/to/checkout \
  --repo-url owner/repo \
  --default-branch main

looper projects list
looper projects get myapp
looper projects sync myapp             # Discover worktrees / PRs
looper projects remove myapp
```

### Loops, runs, queue

Primary workflow today: register a project, label issues `looper:plan` (or `dispatch/plan`), and let the scheduler tick. Low-level CRUD:

```bash
# Loops
looper loops list <project>
looper loops get <project> <seq>
looper loops create <project> --type <type> [--target <target>] [--metadata <json>]
looper loops terminate <project> <seq>

# Runs (per loop seq)
looper runs list <project> <seq>
looper runs get <project> <seq> <run-id>
looper runs start <project> <seq> <run-id> --step <step> [--vendor <v>] [--model <m>]
looper runs cancel <project> <seq>

# Queue
looper queue list <project>
looper queue enqueue <project> --type <type> [--loop-seq <n>] [--priority <p>] [--payload <json>]
looper queue dequeue <project> <item-id>   # single item only
```

Active loops at a glance / stop by sequence:

```bash
looper ps list
looper stop stop <seq> [-p <project>]      # terminate loop by seq
looper jump jump <seq> [-p <project>]      # print worktree path for a loop
```

### Config

```bash
# Daemon-side (via API)
looper config get                      # Full daemon config
looper config agent <project>          # Agent config for a project

# Local file config (no daemon required)
looper config-local get <key>          # e.g. server.host
looper config-local set <key> <value>
looper config-local unset <key>
looper config-local edit
looper config-local migrate
```

### Events, locks, worktrees, PR, bootstrap, upgrade

```bash
looper events list <project>
looper locks list
looper locks acquire <resource> [--ttl <secs>]
looper locks release <resource>

looper worktree cleanup [--dry-run] [--project-id <id>] [--retention-days <n>]

looper pr list [-p <project>]
looper pr show ...
looper pr status ...

looper bootstrap run                   # First-run checks (gh auth, etc.)
looper bootstrap status

looper autoupgrade check               # Check GitHub releases
looper autoupgrade status
looper autoupgrade upgrade             # Manual upgrade path when available
```

### Output formats

Prefer `--json` for scripts. Human output is plain text tables / messages.

---

## Configuration

Looper uses a layered configuration system: embedded defaults -> config file -> environment variables -> CLI overrides.

### Config file discovery

The daemon searches for `looper.toml` (or `.yaml`, `.json`) in order:

1. `$LOOPER_CONFIG/<file>` (explicit, highest priority)
2. `$XDG_CONFIG_HOME/looper/<file>`
3. Platform config dir (`~/.config/looper/` on Linux, `~/Library/Application Support/com.looper.looper/` on macOS)
4. `~/.looper/<file>` (legacy fallback)

### Security features

- Config files with `world-readable` permissions trigger a warning on Unix
- Prefer `looper config get` / `looper config-local get` for inspecting config; avoid putting secrets in committed files
- Sensitive environment variables are stripped before agent subprocess execution
- API authentication via Bearer tokens

### Minimal example

```toml
[server]
host = "127.0.0.1"
port = 7391

[daemon]
data-dir = "~/.looper"
poll-interval-secs = 30

[storage]
db-path = "~/.looper/looper.db"

[github]
gh-path = "/usr/local/bin/gh"

[agent]
vendor = "claude-code"
timeout-secs = 300
```

See [docs/configuration.md](docs/configuration.md) for the full reference (897 lines, every option documented with defaults).

---

## Network Mode (Multi-Node)

LooperNet provides a central cloud coordination server that allows multiple daemon instances to share work across a network.

```
┌──────────────┐       ┌──────────────┐
│ Daemon A     │       │ Daemon B     │
│ (macOS)      │       │ (Linux x86)  │
│              │       │              │
│ 3 workers    │       │ 5 workers    │
└──────┬───────┘       └──────┬───────┘
       │                      │
       └──────────┬───────────┘
                  │
         ┌────────▼────────┐
         │   loopernet     │
         │  (cloud coord)  │
         │                 │
         │ Claim routing   │
         │ Peer discovery  │
         │ State sync      │
         └─────────────────┘
```

- **Node types**: coordinator (leader, one per network) and workers
- **Claim policy**: configurable strategies (earliest-idle, round-robin, affinity)
- **State sync**: periodic heartbeat with full state reconciliation
- **Security**: mutual TLS, join keys, identity management

See [docs/network-mode.md](docs/network-mode.md) for the full protocol and deployment guide.

---

## Development

### Prerequisites

- Rust 1.85+ (stable) — see `rust-toolchain.toml`
- Cargo (included with Rust)
- A GitHub CLI (`gh`) binary for gateway tests
- SQLite (libsqlite3-dev on Linux, bundled crate on macOS)

### Building

```bash
cargo build                    # Debug build
cargo build --release          # Release build (4 targets via rust-toolchain.toml)
cargo check --workspace        # Fast type-check (preferred during development)
```

### Testing

```bash
cargo test --workspace         # Full test suite (unit + integration)
cargo test -p <crate>          # Single crate
cargo test <name>              # Single test by name
cargo test --workspace -- --nocapture  # With output
```

### Linting

```bash
cargo fmt                      # Auto-format
cargo fmt --check              # Format check
cargo clippy --workspace -- -D warnings  # Full lint
```

### CI/CD

| Workflow | Trigger | Jobs |
|----------|---------|------|
| `ci.yml` | Push/PR to main | `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` on 3 OSes (ubuntu, macos, windows) |
| `release.yml` | Tag `v*` | Cross-compile for 4 targets, upload to GitHub Release |

### Crate summary

| Crate | Lines | Role |
|-------|-------|------|
| `looper-types` | ~800 | Domain enums, state machines, core types (zero workspace deps) |
| `looper-config` | ~1,800 | Config loading, TOML/YAML/JSON parsers, validation, merge, secret disclosure |
| `looper-storage` | ~1,500 | SQLite via rusqlite, 17 migrations via refinery, CRUD repos, event log |
| `looper-service` | ~1,200 | Loop/Run/Project state machine transitions, business logic |
| `looper-github` | ~1,800 | GitHub gateway via `gh` CLI, issue/PR CRUD, label management, comments |
| `looper-git` | ~800 | Git worktree management via `git2`, safety checks, cleanup |
| `looper-agent` | ~1,800 | Agent executor for 5 vendors, MCP protocol, output parsing, timeout |
| `looper-scheduler` | ~1,200 | Tick loop, queue claim, issue discovery, priority routing |
| `looper-runner` | ~20,000 | 5 agent roles (Planner/Reviewer/Fixer/Worker/Coordinator), loop orchestration |
| `looper-api` | ~2,500 | axum REST server, Bearer auth, SSE events, 19 error codes, results envelope |
| `looper-webhook` | ~400 | Webhook forwarding, event routing, secret rotation |
| `looper-infra` | ~600 | Bootstrap, runtime lifecycle, file lock, SIGTERM/SIGINT, notifications |
| `looperd` | ~50 | Daemon binary entry point · wires 16 crates together |
| `looper-cli` | ~3,000 | CLI binary (clap), daemon client via reqwest, autoupgrade |
| `looper-net` | ~3,000 | loopernet cloud server, node registration, claim routing, heartbeat |
| `diffanchor` | ~400 | Diff parsing, anchor validation for agent patches |

---

## FAQ

**Does Looper need API keys for every vendor?** No. You provide the agent binary (Claude Code, Codex CLI, etc.) and its own auth; Looper only invokes it.

**Can I run Looper on a single repo?** Yes. Looper works on any number of repos — you configure which repos to watch.

**What happens if an agent hangs or crashes?** Agent processes are wrapped with a configurable timeout (default 5 min per role). On timeout/crash, the run is marked as failed and the error is logged. The daemon continues normally.

**Is the SQLite database safe to delete?** Yes — it stores run history and queue state. Deleting it loses that history, but the daemon re-creates it on next start.

**Does Looper modify my local git state?** Looper uses git worktrees (via `git2`) for all operations. Your primary checkout is never touched.

**How is this different from CI/CD?** CI/CD rejects broken code. Looper *fixes* it — it writes code, pushes PRs, and iterates until checks pass. It is a development teammate, not a gate.

---

## License

MIT — see [LICENSE](LICENSE).

---

*Built with [clap](https://github.com/clap-rs/clap), [tokio](https://tokio.rs), [axum](https://github.com/tokio-rs/axum), [rusqlite](https://github.com/rusqlite/rusqlite), [serde](https://serde.rs), [tracing](https://docs.rs/tracing), and 30+ other Rust crates.*

## Install

### macOS / Linux
```bash
curl -fsSL "https://raw.githubusercontent.com/quangdang46/looper_rust/main/install.sh" | bash
```

### Windows PowerShell
```powershell
irm "https://raw.githubusercontent.com/quangdang46/looper_rust/main/install.ps1" | iex
```

### From Source
```bash
cargo build --release -p looperd -p looper-cli
cp target/release/looperd target/release/looper-cli target/release/looper ~/.local/bin/
```

## Quick Start
```bash
# Start daemon (API on http://127.0.0.1:7391)
looper daemon start
looper health

# Add a project with a resolvable GitHub repo binding
looper projects add my-project \
  --path /path/to/repo \
  --repo-url owner/repo \
  --default-branch main

# Label an issue with `looper:plan` to trigger the pipeline
```

## CI/CD
- CI: `cargo fmt` + `cargo clippy` + `cargo test` on ubuntu/macos/windows
- Release: cross-compile 5 targets on `v*` tag → GitHub Release
