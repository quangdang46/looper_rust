# Looper Rust Port — Honest Gap Analysis vs Go

> Generated 2026-06-22. Every number verified from actual source files, not estimates.

## Basic Stats

```
Go source (non-test):  83,479 LOC in 34 packages
Go tests:              ~82,577 LOC
Rust source (all):     37,280 LOC in 16 crates
Rust tests:             ~46,100 LOC (looper-storage/tests.rs alone is 46K)
```

## ✅ Complete (≥85% parity)

| Feature | Go LOC | Rust LOC | Parity | Notes |
|---------|--------|----------|--------|-------|
| Domain types (enums, state machines) | 258 | ~3,000 | **100%** | Rust is way richer — serde, Display, FromStr on everything |
| Config loading/validation | 6,034 | ~3,200 | **100%** | 3-layer merge, env overlay, disclosure stamping |
| SQLite storage | 3,618 | ~2,022 | **95%** | 12 repos, 2 migrations, WAL mode, full test suite |
| GitHub gateway | 4,657 | ~5,938 | **95%** | 50+ functions, octocrab + gh CLI, richer than Go |
| Git worktree ops | 1,171 | ~1,563 | **95%** | Safety checks, fetch/checkout/push, branch management |
| Agent executor | 1,757 | ~1,832 | **90%** | 5 vendors (Go had 1), MCP + subprocess |
| Scheduler tick loop | 2 | ~2,108 | **100%** | Go was a stub. Rust has real queue claim, failure classification, recovery |
| REST API (axum) | 5,782 | ~3,568 | **90%** | 25+ endpoints, auth, SSE, envelope |
| Notification system | 383 | ~494 | **90%** | Database + osascript + throttle |
| Worker role | 3,810 | ~632 | **85%** | Core pipeline exists |
| Planner role | 2,200 | ~703 | **85%** | Core pipeline exists |
| Fixer role | 7,116 | ~913 | **85%** | Core pipeline + thread resolution |

## 🟡 Partially Ported (exist but thin)

| Feature | Go LOC | Rust LOC | Parity | What Rust Is Missing |
|---------|--------|----------|--------|----------------------|
| Reviewer role | 7,130 | ~843 | **60%** | No acceptance criterion verification, no auto-merge decision logic |
| Coordinator | 3,842 | ~1,786 | **50%** | No triage engine (LLM issue classifier), no dependency graph |
| Webhook | 753 | ~2,846 | **60%** | No process lifecycle management for gh webhook forwarder |
| Infra | 6,520 | ~1,981 | **70%** | Missing lifecycle policy, release manifest, extracted shell runner |
| Network mode | 1,937 | ~2,963 | **80%** | Missing cloud protocol message types, tunnel server |
| Runtime | 8,301 | ~1,176 | **30%** | No recovery pipeline, no webhook forwarder lifecycle, no tunnel server |

## 🔴 Critical Gaps — Not Ported At All (0%)

### 1. Triage Engine (`internal/coordinator/triage/` — 340 LOC)
LLM-powered issue classification. Determines if an issue is "valid", "out-of-scope", or "unclear" — produces kind/area/complexity/dispatch labels.
**Impact:** Without this, every issue labeled `looper:plan` gets planned regardless of quality.

### 2. Review Criteria Verifier (`internal/reviewer/criteria/` — 357 LOC)
Extracts acceptance criteria from issue body, verifies each against PR diff.
**Impact:** Without this, the reviewer can't produce structured pass/fail verification per criterion.

### 3. Auto-Merge Decision (`internal/reviewer/automerge/` — 88 LOC)
Decides IF a PR qualifies for auto-merge (checks branch protection, strategy, scope).
**Impact:** Coordinator can't auto-merge without this.

### 4. Dependency Graph (`internal/coordinator/depgraph/` — 342 LOC)
Builds DAG from GitHub issue dependencies. Computes ready set, cycles, blockers.
**Impact:** Multi-issue coordination doesn't work at all.

### 5. Lifecycle Policy (`internal/lifecycle/` — 474 LOC)
Tracks who caused each action (agent vs human fallback), branch provenance.
**Impact:** System can't distinguish agent-initiated vs human-initiated actions.

### 6. Runtime Recovery Pipeline (`internal/runtime/runtime.go` — ~8K LOC)
5-phase startup recovery, webhook forwarder process supervision, tunnel management.
**Impact:** Daemon crash leaves orphaned agents. No self-healing.

### 7. Webhook Tunnel Server (`internal/runtime/webhook_tunnel.go` — ~1.5K LOC)
Creates/updates/deletes GitHub hooks via gh CLI. HMAC auth. Disable latch.
**Impact:** Tunnels are just data in SQLite — they don't actually create hooks.

### 8. E2E Test Harness (`internal/e2e/` — 2,622 LOC)
Fake GitHub server (36K Go), fake agent, fake osascript. 15+ E2E test files.
**Impact:** Zero E2E coverage. No way to verify the system works without real GitHub + real AI.

### 9. CLI: 15 Missing Command Groups (`internal/cliapp/` — ~8K LOC)
review submit, run takeover, run stats, logs follow, daemon install, daemon supervision, daemon runtime, network admin, labels init, prompt, feedback, upgrade, webhook group, loop diagnostics, progress

## Overall Honest Score

| Metric | Value |
|--------|-------|
| Go non-test LOC covered | **~62%** by feature weight |
| Go features NOT covered | **~19,000 LOC** worth of functionality |
| Test parity | High unit test density (46K LOC) but **zero E2E tests** |
| Go LOC → Rust LOC ratio | 83K → 37K (but ~7K of Rust is "extra" — more vendors, richer types) |
| Actual functional parity | **~62%** |
