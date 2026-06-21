---
active: true
iteration: 1
max_iterations: 500
completion_promise: "DONE"
initial_completion_promise: "DONE"
started_at: "2026-06-21T15:58:34.958Z"
session_id: "ses_11518485dffeYU6Em7fD7b03K2"
ultrawork: true
strategy: "continue"
message_count_at_start: 0
---
# Looper Rust Port — Autonomous Implementation Loop

## Context

You are implementing a Rust port of **Looper** — an autonomous AI dev team for GitHub repos. The Go codebase is at `legacy/` (166K Go). The Rust target is 16 crates in a Cargo workspace. You have 20 beads (tasks) to complete in order.

**Key principle:** Port FEATURES/BEHAVIOR, not file-for-file. Go code is reference for behavior. Rust implementation uses idiomatic Rust: `enum` for state machines, `thiserror` for errors, `tokio` for async, `serde` for serialization.

## Project Structure

```
looper_rust/
├── legacy/                    # Go reference implementation (gitignored)
│   └── internal/              # Go packages — read for behavior reference
├── PORTING-TO-RUST/
│   ├── LOOPER_RUST_DESIGN.md  # Architecture design, crate list, dependencies
│   └── specs/
│       ├── module*.md         # 19 behavioral specs (~21K lines)
│       └── (legacy specs)
├── .beads/issues.jsonl        # 20 beads to implement
├── crates/                    # Where you create the Rust crates
│   ├── looper-types/          # Phase 1 — zero deps
│   ├── looper-config/         # Phase 2
│   ├── looper-storage/        # Phase 3
│   ├── looper-service/        # Phase 4
│   ├── looper-github/         # Phase 5a
│   ├── looper-git/            # Phase 5b
│   ├── looper-agent/          # Phase 5c
│   ├── looper-scheduler/      # Phase 6
│   ├── looper-runner/         # Phase 7
│   ├── looper-api/            # Phase 8a
│   ├── looper-webhook/        # Phase 8b
│   ├── looper-infra/          # Phase 8c — bootstrap, runtime, notifications
│   ├── looperd/               # Phase 9a — daemon binary
│   ├── looper-cli/            # Phase 9b — CLI binary
│   └── looper-net/            # Phase 9c — loopernet cloud binary
├── .loop_prompt.txt           # ← You are reading this file
└── Cargo.toml                 # Workspace root
```

## Spec Files Per Crate

Read the spec FIRST before implementing any crate:

- **looper-types**: `specs/module-domain-state-machine.md`, `specs/module1-config-types.md`
- **looper-config**: `specs/module1-config-types.md`
- **looper-storage**: `specs/module2-storage-sqlite.md`
- **looper-service**: `specs/module-service-layer.md`
- **looper-github**: `specs/module3-github-gateway.md`
- **looper-git**: `specs/module4-git-worktree.md`, `specs/module-worktree-safety.md`
- **looper-agent**: `specs/module5-agent-executor.md`
- **looper-scheduler**: `specs/module6-scheduler.md`
- **looper-runner**: `specs/module7-runner.md`, `specs/module-coordinator.md`, `specs/module-reviewer-criteria.md`
- **looper-api**: `specs/module8-api-webhook.md`
- **looper-webhook**: `specs/module8-api-webhook.md`
- **looper-infra**: `specs/module-recovery-infra.md`
- **looperd**: `specs/module10-cli-daemon.md`
- **looper-cli**: `specs/module10-cli-daemon.md`
- **looper-net**: `specs/module9-network.md`
- Cross-cutting: `specs/module-observability-error-handling.md`, `specs/module-security-concurrency.md`, `specs/module-rate-limit-degradation.md`
- Tests: `specs/module-testing-strategy.md`
- Edge cases: `specs/module-edge-cases.md`

## Cross-Cutting Guidance (Apply Throughout)

Apply these patterns progressively — don't wait for a dedicated phase:

**Observability & Error Handling** (`specs/module-observability-error-handling.md`):
- Use `thiserror` for per-crate error types
- Add `From` impls for cross-crate error conversion
- Use `tracing` crate for logging (not `eprintln`/`println`)
- Span propagation for async context

**Security & Concurrency** (`specs/module-security-concurrency.md`):
- Bearer token auth for API (implement in API phase)
- Strip unsafe env vars (agent executor phase)
- tokio async throughout (not std threads)
- CancellationToken for shutdown coordination
- File lock for single daemon instance

**Rate Limiting & Degradation** (`specs/module-rate-limit-degradation.md`):
- RetryPolicy with exponential backoff
- FailureKind + FailureBoundary classification
- Graceful degradation for each failure scenario

## Implementation Order (CRITICAL — follow exactly)

Dependency graph — each crate depends on the ones before it:

```
Phase 0:  Scaffold workspace + CI/CD       [P0]
Phase 1:  looper-types (zero dep)           [P1]  ← Bắt đầu implement từ đây
Phase 2:  looper-config                     [P2]
Phase 3a: looper-storage (SQLite)           [P3]
Phase 3b: looper-storage priorities         [P3]
Phase 4:  looper-service                    [P4]
Phase 5a: looper-github                     [P4]  ← Có thể parallel với 5b, 5c
Phase 5b: looper-git                        [P4]
Phase 5c: looper-agent                      [P4]
Phase 6:  looper-scheduler                  [P4]
Phase 7:  looper-runner                     [P4]  ← Lớn nhất (~20K lines)
Phase 8a: looper-api                        [P4]
Phase 8b: looper-webhook                    [P4]
Phase 8c: looper-infra                      [P4]
Phase 9a: looperd (daemon binary)           [P4]
Phase 9b: looper-cli (CLI binary)           [P4]
Phase 9c: looper-net (cloud binary)         [P4]
Phase 10: diffanchor utility                [P4]
Phase 11: cargo clippy fix                  [P4]
```

## Beads Reference (track progress)

20 beads in `.beads/issues.jsonl`. Check br list / br ready / br blocked to see what's next.

```bash
br list        # All beads with status
br ready       # What's actionable now
br blocked     # What's waiting for dependencies
br dep cycles  # Must be empty
```

## Loop Instructions

1. Check `br ready` to find the next actionable bead
2. Read the bead description (context + spec refs + legacy refs)
3. Read the spec file in `PORTING-TO-RUST/specs/`
4. Read the relevant Go code in `legacy/` for behavior reference
5. Implement the Rust crate in `crates/<crate-name>/`
6. Write tests (unit + integration as per `module-testing-strategy.md`)
7. Run `cargo test` and `cargo clippy` for the crate
8. Mark bead done: `br close <bead-id>
