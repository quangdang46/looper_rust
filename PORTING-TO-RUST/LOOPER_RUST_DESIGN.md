# Looper Rust — Full Feature Port Architecture

## Project Structure

```
looper/
├── Cargo.toml                    # Workspace root (13 crates)
├── Cargo.lock
├── rust-toolchain.toml           # stable, 4 targets
├── rustfmt.toml                  # Format config
├── clippy.toml                   # Lint config
├── .gitignore
├── .github/workflows/
│   ├── ci.yml                    # fmt + clippy + test, 3-OS matrix
│   └── release.yml               # cross-compile 4 targets on vX.Y.Z tag
├── AGENTS.md                     # AI coding agent guidelines
├── LICENSE                       # MIT
├── README.md
├── docs/
│   ├── architecture.md           # Tổng quan kiến trúc
│   ├── configuration.md          # Config tham khảo
│   ├── agent-vendors.md          # Cách thêm agent vendor
│   ├── network-mode.md           # Multi-node setup
│   └── specs/                    # Ported from Go Looper's 76 specs
│       └── ... (76 files)
├── install.sh                    # Linux/macOS installer (curl | bash)
├── skills/
│   ├── looper/
│   │   └── SKILL.md
│   └── looper-qa/
│       ├── SKILL.md
│       └── references/
│           └── test-suites.md
├── configs/
│   └── looper.toml               # Default config (1 format duy nhất)
└── crates/
    ├── looper-core/              # Core types, config, errors
    ├── looper-storage/           # SQLite database
    ├── looper-github/            # GitHub API (octocrab)
    ├── looper-git/               # Git worktree (git2)
    ├── looper-agent/             # Agent executor (MCP + subprocess)
    ├── looper-scheduler/         # Tick loop + queue
    ├── looper-runner/            # Planner + Reviewer + Fixer + Worker + Coordinator
    ├── looper-api/               # REST API (axum)
    ├── looper-network/           # Multi-node mode
    ├── looper-webhook/           # Webhook forwarder
    ├── looperd/                  # Daemon binary
    ├── looper-cli/               # CLI binary
    └── looper-net/               # loopernet binary
```

## Workspace Cargo.toml

```toml
[workspace]
resolver = "2"
members = [
    "crates/looper-core",
    "crates/looper-storage",
    "crates/looper-github",
    "crates/looper-git",
    "crates/looper-agent",
    "crates/looper-scheduler",
    "crates/looper-runner",
    "crates/looper-api",
    "crates/looper-network",
    "crates/looper-webhook",
    "crates/looperd",
    "crates/looper-cli",
    "crates/looper-net",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT"
authors = ["quangdang46"]
repository = "https://github.com/quangdang46/looper"
description = "Autonomous AI dev team for your GitHub repos — Rust port"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
toml = "0.8"
clap = { version = "4", features = ["derive", "env"] }
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }
rusqlite = { version = "0.33", features = ["bundled"] }
refinery = { version = "0.9", features = ["rusqlite"] }
octocrab = "0.43"
git2 = "0.20"
rmcp = { version = "1.3", features = ["transport-io"] }
axum = "0.8"
uuid = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", features = ["serde"] }
regex = "1"
directories = "6"
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json"] }
```

## Build Commands

```bash
# Build
cargo build --release

# Check
cargo check --all-targets

# Test
cargo test --workspace

# Lint
cargo fmt --check
cargo clippy --workspace -- -D warnings

# Install
cp target/release/looperd ~/.local/bin/
cp target/release/looper ~/.local/bin/
```

## rust-toolchain.toml

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
targets = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
]
```

## CI/CD Workflows

### ci.yml

```yaml
name: CI
on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
env:
  CARGO_TERM_COLOR: always
jobs:
  check:
    name: Check (${{ matrix.os }})
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all -- --check
      - run: cargo clippy --all-targets --all-features -- -D warnings
      - run: cargo test --all-features
```

### release.yml

Trigger: `vX.Y.Z` tag → cross-compile → upload to GitHub Release.

```yaml
name: Release
on:
  push:
    tags: ["v*"]
permissions:
  contents: write
jobs:
  build:
    name: Build (${{ matrix.target }})
    strategy:
      matrix:
        include:
          - target: x86_64-unknown-linux-musl
            os: ubuntu-latest
            suffix: linux-x86_64
          - target: aarch64-unknown-linux-musl
            os: ubuntu-latest
            suffix: linux-aarch64
          - target: x86_64-apple-darwin
            os: macos-latest
            suffix: macos-x86_64
          - target: aarch64-apple-darwin
            os: macos-latest
            suffix: macos-aarch64
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - uses: Swatinem/rust-cache@v2
      - run: cargo build --release --target ${{ matrix.target }}
        env:
          LOOPER_CHANNEL: stable
      - name: Package
        shell: bash
        run: |
          for bin in looperd looper looper-net; do
            src="target/${{ matrix.target }}/release/$bin"
            [ -f "$src" ] || continue
            mkdir -p dist
            out="dist/${bin}-${{ matrix.suffix }}"
            cp "$src" "$out"
            sha256sum "$out" > "${out}.sha256"
          done
      - uses: actions/upload-artifact@v4
        with:
          name: artifacts-${{ matrix.suffix }}
          path: dist/*
  release:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - uses: actions/download-artifact@v4
      - name: Prepare assets
        run: |
          mkdir -p assets
          find . -type f \( -name "*.sha256" -o -name "looper-*" \) -exec cp {} assets/ \;
      - uses: softprops/action-gh-release@v2
        with:
          generate_release_notes: true
          files: assets/*
```

### Cách release

```bash
git tag v0.1.0
git push origin v0.1.0
# → CI build 4 targets → tạo GitHub Release tự động
```

## Kiến trúc tổng thể

```
người dùng
  │ looper CLI (clap)
  ▼
┌──────────────────────────────────────────────────────────────────┐
│  looperd (daemon, tokio runtime)                                 │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │  Scheduler (tokio::time::interval 30s)                     │  │
│  │  Planner → Queue  |  Reviewer → Queue                      │  │
│  │  Fixer → Queue     |  Worker → Queue                       │  │
│  │  Coordinator → triage → dispatch                           │  │
│  └────────────────────────────────────────────────────────────┘  │
│                              │ queue items                       │
│                              ▼                                   │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │  Queue Manager (rusqlite + mpsc)                           │  │
│  │  priorities: planner=1, reviewer/fixer=2, worker=3         │  │
│  │  max_concurrent: 3 (configurable)                          │  │
│  └────────────────────────────────────────────────────────────┘  │
│                              │ claim                             │
│                              ▼                                   │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │  Runner Pool (tokio::task::JoinSet)                        │  │
│  │  Planner:    Discover → Filter → Claim → Snapshot →        │  │
│  │              Worktree → WriteSpec → OpenPR                  │  │
│  │  Reviewer:   Discover → Filter → Claim → Snapshot →        │  │
│  │              Worktree → ThreadResolution → Review → Publish │  │
│  │  Fixer:      Discover → Claim → CollectFixes → Worktree →  │  │
│  │              Repair → Validate → Push → ResolveComments     │  │
│  │  Worker:     Claim → Snapshot → Worktree → Implement →     │  │
│  │              Validate → Push                                 │  │
│  │  Coordinator: Discover → Triage (LLM) → Dispatch →          │  │
│  │               MergeWatch                                     │  │
│  └────────────────────────────────────────────────────────────┘  │
│                                                                  │
│  Shared: looper-github (octocrab) | looper-git (git2)            │
│          looper-storage (rusqlite) | looper-agent (MCP)          │
│          looper-api (axum) | looper-webhook (axum)               │
└──────────────────────────────────────────────────────────────────┘
```

## Core Data Types

```rust
// Config
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub scheduler: SchedulerConfig,
    pub webhook: WebhookConfig,
    pub agent: AgentConfig,
    pub logging: LoggingConfig,
    pub notifications: NotificationConfig,
    pub tools: ToolPathsConfig,
    pub daemon: DaemonConfig,
    pub roles: RoleConfigs,
    pub projects: Vec<ProjectRefConfig>,
}

// Storage records
pub struct ProjectRecord { pub id: String, pub name: String, pub repo_path: String, /* ... */ }
pub struct LoopRecord    { pub id: String, pub project_id: String, pub loop_type: LoopType, /* ... */ }
pub struct RunRecord     { pub id: String, pub loop_id: String, pub status: RunStatus, /* ... */ }
pub struct QueueItemRecord { pub id: String, pub priority: QueuePriority, pub status: QueueStatus, /* ... */ }

// GitHub
pub struct Gateway { client: octocrab::Octocrab, cache: Cache, /* ... */ }
impl Gateway {
    // Issues: list, get, create, edit, labels, assignee
    // PRs: list, get, create, merge, auto-merge
    // Reviews: submit (APPROVE|COMMENT|REQUEST_CHANGES)
    // Comments: list, create
    // Checks: list, branch-protection
}

// Agent
pub trait AgentExecutor: Send + Sync {
    async fn execute(&self, input: AgentInput) -> Result<AgentOutput>;
}

pub struct HermesMCP { client: rmcp::Client }
pub struct ClaudeCodeExecutor;  // subprocess
pub struct CodexExecutor;       // subprocess
pub struct OpenCodeExecutor;    // subprocess
pub struct CursorCliExecutor;   // subprocess
```

## Data Flow: Full Issue → Merge

```
1. Issue label "looper:plan" → Coordinator triage → dispatch
2. Planner: snapshot issue → write spec → open spec PR
3. Reviewer: review spec → approve → label "looper:spec-ready"
4. Worker: implement spec → push code → re-review
5. Reviewer ↔ Fixer loop: review → fix → review → approve
6. Merge
```

## Feature Parity Checklist

| # | Feature | Go LOC | Rust Crate |
|---|---|---|---|
| 1 | Config load/save (toml) | 10.5K | looper-core |
| 2 | Config validation | — | looper-core |
| 3 | SQLite storage (13 repos) | 7.2K | looper-storage |
| 4 | DB migrations (refinery) | — | looper-storage |
| 5 | GitHub Issues CRUD | — | looper-github |
| 6 | GitHub PRs CRUD | 3.7K | looper-github |
| 7 | GitHub Reviews submit | — | looper-github |
| 8 | GitHub Checks + Branch protection | — | looper-github |
| 9 | Discovery cache | — | looper-github |
| 10 | Git worktree add/remove/list | 1.5K | looper-git |
| 11 | Git fetch PR + create branch | — | looper-git |
| 12 | Git push + commit | — | looper-git |
| 13 | Hermes MCP executor | 1.7K | looper-agent |
| 14 | Claude Code executor | — | looper-agent |
| 15 | Codex executor | — | looper-agent |
| 16 | OpenCode executor | — | looper-agent |
| 17 | Cursor CLI executor | — | looper-agent |
| 18 | Native resume (all venders) | — | looper-agent |
| 19 | Scheduler tick loop | 1.8K | looper-scheduler |
| 20 | Queue claim + priority | — | looper-scheduler |
| 21 | Running tracker (max concurrent) | — | looper-scheduler |
| 22 | Planner state machine | 2.2K | looper-runner |
| 23 | Reviewer state machine | 6.2K | looper-runner |
| 24 | Fixer state machine | 7.1K | looper-runner |
| 25 | Worker state machine | 3.8K | looper-runner |
| 26 | Coordinator (triage + LLM) | 5.5K | looper-runner |
| 27 | Merge-watch | — | looper-runner |
| 28 | Auto-merge logic | — | looper-runner |
| 29 | Thread resolution | — | looper-runner |
| 30 | Spec PR management | 0.5K | looper-runner |
| 31 | REST API (25+ endpoints) | 5.5K | looper-api |
| 32 | Auth (none + local-token) | — | looper-api |
| 33 | Webhook forwarder (reviewer/fixer lanes) | 3K | looper-webhook |
| 34 | Webhook tunnel | — | looper-webhook |
| 35 | Network client | — | looper-network |
| 36 | Network cloud registration | — | looper-network |
| 37 | Network protocol | — | looper-network |
| 38 | Network claim policy | — | looper-network |
| 39 | CLI: plan/review/fix/work | — | looper-cli |
| 40 | CLI: takeover | — | looper-cli |
| 41 | CLI: ps/logs/stop | — | looper-cli |
| 42 | CLI: project/config/daemon | — | looper-cli |
| 43 | CLI: queue/labels/bootstrap | — | looper-cli |
| 44 | CLI: status/version/upgrade | — | looper-cli |
| 45 | Daemon bootstrap + recovery | — | looperd |
| 46 | Signal handling (SIGTERM, SIGINT) | — | looperd |
| 47 | macOS notifications (notify-rust) | — | looperd |
| 48 | Disclosure stamps | 0.3K | looper-core |
| 49 | Label lifecycle helpers | — | looper-core |
| 50 | Diff anchoring | 0.5K | looper-runner |
| 51 | CI workflow (ci.yml) | — | .github/ |
| 52 | Release workflow (release.yml) | — | .github/ |
| 53 | Install script (curl\|bash) | — | install.sh |
| 54 | Version info (build-time) | 1K | looper-core |
| 55 | Upgrade CLI command | 1.6K | looper-cli |
| 56 | Auto-upgrade daemon | — | looperd |
| 57 | Release via tag (CI/CD) | — | .github/ |
| 58 | Channel support (stable/beta) | — | looper-core |
| 59 | Atomic binary replace | — | looper-cli |
| 60 | E2E tests (mock GitHub + agent) | 280K Go | tests/e2e/ |
| 61 | Integration tests (all runners) | — | tests/integration/ |

**Total: 61 features | Go: ~166K LOC | Rust: ~50K LOC**

## Implementation Order

| Phase | Tuần | Crates | Deliverable |
|---|---|---|---|
| 1 | 1 | core, storage | Config load, DB schema, migrations |
| 2 | 1 | github, git | GitHub API, git worktree, tests |
| 3 | 1 | agent | All 5 venders, MCP Hermes |
| 4 | 1 | scheduler | Tick loop, queue, claim, tracker |
| 5 | 2 | runner (planner, worker) | Planner + Worker state machines |
| 6 | 2-3 | runner (reviewer, fixer) | Reviewer + Fixer + thread resolution |
| 7 | 1 | runner (coordinator) | Triage + dispatch + merge-watch |
| 8 | 1 | api, webhook | REST API + webhook handler |
| 9 | 1 | network | Network mode |
| 10 | 1 | cli, looperd | CLI commands, daemon bootstrap |
| 11 | 1 | cli (net) | loopernet binary |
| 12 | 2 | tests/e2e | E2E harness + full cycle tests |
| 13 | 1 | docs, ci, install | mdBook, GitHub Actions, install scripts |

**Total: ~14 tuần (3.5 tháng) full-time**
