# Looper Rust — AI Agent Guidelines

## Build

```bash
cargo build --release          # Release build (all targets)
cargo build                    # Debug build
cargo check --workspace        # Fast type-check (recommended during dev)
```

## Test

```bash
cargo test --workspace         # All tests
cargo test -p <crate>          # Single crate
cargo test -p <crate> -- <name> # Single test by name
```

## Lint

```bash
cargo fmt --check              # Format check
cargo fmt                      # Auto-format
cargo clippy --workspace -- -D warnings  # Full lint
cargo clippy -p <crate>        # Single crate
```

## Workspace Members

| Crate | Type | Description |
|-------|------|-------------|
| looper-types | lib | Domain enums, state machines, core types |
| looper-config | lib | Config loading, validation, merge, disclosure |
| looper-storage | lib | SQLite repositories, migrations, event log |
| looper-service | lib | Loop, Run, Project business logic |
| looper-github | lib | GitHub gateway via gh CLI |
| looper-git | lib | Git worktree management |
| looper-agent | lib | Agent executor (5 vendors) |
| looper-scheduler | lib | Tick loop, queue claim, discovery |
| looper-runner | lib | Planner, Reviewer, Fixer, Worker, Coordinator |
| looper-api | lib | axum REST server, auth, envelope |
| looper-webhook | lib | Webhook forwarder, event routing |
| looper-infra | lib | Bootstrap, runtime, notifications |
| looperd | bin | Daemon binary |
| looper-cli | bin | CLI binary |
| looper-net | bin | Loopernet cloud server binary |
| diffanchor | lib | Diff parsing and anchor validation |

## Directory Layout

```
looper_rust/
├── Cargo.toml                 # Workspace root
├── crates/                    # All crates
│   ├── looper-types/          # Phase 1
│   ├── looper-config/         # Phase 2
│   ├── ...
├── legacy/                    # Go reference (gitignored)
├── PORTING-TO-RUST/
│   ├── LOOPER_RUST_DESIGN.md  # Architecture design
│   └── specs/                 # Behavioral specs
├── .github/workflows/         # CI/CD
└── .beads/                    # Task tracking
```
