# Beads — Full Feature Parity + Treehouse Integration

## Phase 0: Quick Wins

- [ ] **Phase 0: Documentation & Repo Hygiene** `epic` `p2`
  Port all documentation, skills, templates, and repo hygiene from Go original.

  ## Scope
  - Port 16 ADRs from `.tmp/looper/docs/adr/` to `docs/adr/`
  - Create `CHANGELOG.md` + `docs/changelogs/`
  - Port PR template (`.github/pull_request_template.md`)
  - Port code review checklist (`.github/code-review-checklist.md`)
  - Create issue templates (bug report, feature request)
  - Port `skills/looper/SKILL.md` + references (`cli.md`, `config.md`, `daemon.md`)
  - Port `skills/pr-takeover/SKILL.md` + references
  - Create `.githooks/pre-commit` (cargo fmt + cargo clippy)
  - Create `scripts/verify.sh` (fmt + clippy + test + build)

  ## Justification
  Zero-risk improvements. Gives documentation parity with Go original and improves contributor experience. No code changes = no risk of regression.

  ## Success Criteria
  - All 16 ADRs present in `docs/adr/`
  - All skills present in `skills/`
  - Scripts executable and tested
  - `CHANGELOG.md` tracks from first release
  - `cargo fmt --check && cargo clippy --lib` passes after pre-commit hook

## Phase 1: Takeover Feature (P0)

- [ ] **Phase 1: Takeover Feature** `epic` `p0`
  Single-PR takeover — focus daemon on driving one PR to merge.

  ## Background
  The takeover feature allows an operator to say "focus all resources on this one PR and don't stop until it's merged." It creates reviewer+fixer loops on a specific PR and runs them in a continuous cycle.

  ## Architecture (from Go)
  1. CLI parses `<owner/repo>#<number>` or auto-detects via `gh pr view`
  2. CLI creates/patches config with scoped project entry (auto-discovery disabled)
  3. Starts daemon if down, restarts if config changed
  4. Starts reviewer+fixer loops via API
  5. Records state in `~/.looper/takeovers.json`
  6. CLI resume: daemon returns `{vendor, sessionId, worktreePath, resumeCommand}` → exec into agent
  7. Handback: daemon resumes, sees human's turns

  ## Scope — Domain Types
  Create `crates/looper-types/src/takeover.rs`:
  - `TakeoverSession` (id, project_name, repo_owner, repo_name, pr_number, status, started_at, last_activity, cycles_completed, current_phase, error_count, max_errors)
  - `TakeoverStatus` enum: Active, Paused, Completed, Failed, Cancelled
  - `TakeoverPhase` enum: Planning, Reviewing, Fixing, Working, Merging, Waiting
  - `TakeoverResult` for completion output

  ## Scope — Storage
  Migration V6: `takeover_sessions` table
  - id TEXT PRIMARY KEY, project_name TEXT, repo_owner TEXT, repo_name TEXT, pr_number INTEGER, status TEXT, started_at TEXT, last_activity TEXT, cycles_completed INTEGER, current_phase TEXT, error_count INTEGER, max_errors INTEGER
  - New `TakeoverSessionsRepository` with CRUD + active session lookup

  ## Scope — Runner
  Create `crates/looper-runner/src/takeover.rs`:
  - `TakeoverRunner` struct with session, repos, gateway, executor, config
  - Main loop: cycle through Review→Fix→Check→Repeat until merged/failed/cancelled
  - Phase management: update session.current_phase on each transition
  - Error counting with max_errors threshold
  - PR merged detection via gateway

  ## Scope — CLI Commands
  ```
  looper takeover <ref> --merge --agent-vendor <v>   # Start
  looper takeover list                                # List active
  looper takeover stop <ref|all>                      # Stop
  ```

  ## Scope — API Endpoints
  ```
  POST   /api/projects/{name}/takeover      # Start takeover
  GET    /api/projects/{name}/takeover      # Get status
  DELETE /api/projects/{name}/takeover      # Stop
  GET    /api/takeovers                      # List all active
  ```

  ## Scope — Skills
  Port `skills/pr-takeover/SKILL.md` with both modes:
  - Live mode (in-session, uses gh+git)
  - Background mode (hands off to looper takeover daemon)
  - GraphQL recipes for review thread resolution, change request dismissal

  ## Success Criteria
  - `looper takeover owner/repo#123 --merge` starts reviewer+fixer loops
  - `looper takeover list` shows active takeovers with live status
  - `looper takeover stop owner/repo` stops and cleans up
  - Dashboard shows takeover sessions
  - E2E test: takeover → cycles → merge → cleanup

## Phase 2: Web Dashboard (P0)

- [ ] **Phase 2: Web Dashboard** `epic` `p0`
  React+Vite+TypeScript SPA served by looperd. Dashboard code already copied from Go original; needs daemon integration.

  ## Status
  Dashboard code is already at `crates/looperd/dashboard/` (copied from Go, API paths adapted).
  `crates/looper-api/src/dashboard.rs` provides flat `/api/*` compatibility routes.
  Vite configured to proxy to Rust daemon port 7391.

  ## Remaining Work — Daemon Integration
  1. Add `rust-embed` to `looperd` Cargo.toml
  2. Create `crates/looperd/src/dashboard.rs` — serve embedded SPA assets
  3. Wire dashboard routes into daemon bootstrap
  4. Build integration: `cd crates/looperd/dashboard && pnpm build` before `cargo build`

  ## Remaining Work — Stub Implementations
  - `start_loop_flat`: Wire to actual resume logic (repos.loops.update_status + trigger_scheduler_tick)
  - `pause_loop_flat`: Wire to actual stop logic
  - `retry_loop`: Wire to actual resume + requeue
  - `stop_active_run`: Wire to actual stop logic
  - Config PATCH endpoint: Implement field-level config update
  - Log streaming SSE: Connect to actual agent stdout/stderr

  ## Success Criteria
  - `cargo build -p looperd` includes dashboard assets
  - Dashboard serves at `http://127.0.0.1:7391/dashboard/`
  - Overview page shows loop status counts
  - Loops page lists all loops with filtering
  - LoopDetail page shows runs and agent output
  - Projects page lists projects
  - Config page shows config with editing

## Phase 3: Forgejo/Gitea Provider (P1)

- [ ] **Phase 3: Forge Abstraction + Forgejo Provider** `epic` `p1`
  Abstract forge interface, implement Forgejo/Gitea alongside GitHub.

  ## Background
  The Go original has a `Provider` interface with capabilities (Issues, PullRequests, Labels, Reviews, AutoMerge, Webhooks, etc.) and three implementations: GitHub, Forgejo, Plane. Rust currently only has GitHub via `gh` CLI.

  ## Architecture
  Create `crates/looper-forge/` with:
  - `Forge` trait: repository, issues, PRs, reviews, labels, webhooks
  - `Capabilities` struct: bool flags + strategy enums (ReviewDiscovery, ReviewPublish, ThreadResolution, WorkerClaim, Webhook)
  - `GitHubForge`: wraps existing `looper-github` crate
  - `ForgejoForge`: REST API client
  - `GiteaForge`: REST API client (similar to Forgejo)

  ## Capabilities Matrix
  | Capability | GitHub | Forgejo | Gitea |
  |---|---|---|---|
  | Issues | Y | Y | Y |
  | PullRequests | Y | Y | Y |
  | Labels | Y | Y | Y |
  | AutoMerge | Y | N | N |
  | Webhooks | Y (native) | N (polling) | N (polling) |
  | ReviewCommentResolution | native | manual_only | manual_only |
  | WorkerClaim | assign_self | pre_assigned | pre_assigned |

  ## Migration Path
  1. Define Forge trait
  2. Wrap existing looper-github as GitHubForge
  3. Refactor gateway calls to go through Forge trait
  4. Implement ForgejoForge
  5. Implement GiteaForge
  6. Config routing: project.provider → Forge implementation

  ## Config
  ```toml
  [[projects]]
  provider = "forgejo"
  [projects.provider]
  type = "forgejo"
  url = "https://git.example.com"
  token = "${FORGEJO_TOKEN}"
  ```

  ## Success Criteria
  - Forge trait compiles with all methods
  - GitHubForge passes existing tests
  - ForgejoForge can list issues and create PRs against a test Forgejo instance
  - Config routes to correct provider based on project.provider

## Phase 4: HITL Workflows (P1)

- [ ] **Phase 4: HITL (Human-in-the-Loop) Workflows** `epic` `p1`
  Human decision points for complex scenarios the daemon can't resolve autonomously.

  ## Background
  When the daemon encounters merge conflict, failing CI after multiple attempts, or ambiguous reviewer feedback, it should pause and ask a human for direction.

  ## Trigger Points
  - Merge conflict after fix attempts
  - Failing CI after N retry attempts
  - Ambiguous reviewer feedback
  - Manual intervention needed (local worktree issues)
  - Policy violation

  ## Transport Implementations
  1. **GitHub Comment**: Listen for `/looper retry|merge|skip|abort` on PR
  2. **Slack**: Post message with action buttons, listen for replies
  3. **Webhook**: POST to configured URL, wait for callback
  4. **Dashboard**: SSE event → UI button → POST decision

  ## Scope — Domain Types
  Create `crates/looper-runner/src/hitl.rs`:
  - `HitlDecision` enum: Retry, Skip, Merge, Abort, Custom(String)
  - `HitlRequest` struct: run_id, loop_id, project_name, reason, context, created_at, expires_at
  - `HitlReason` enum: MergeConflict, FailingCi, AmbiguousFeedback, ManualIntervention, PolicyViolation
  - `HitlTransport` trait: request_decision, check_response

  ## Scope — API Endpoints
  ```
  POST /api/projects/{name}/loops/{seq}/respond   # Submit HITL answer
  ```

  ## Success Criteria
  - HITL triggers on merge conflict after 3 fix attempts
  - GitHub comment transport detects `/looper retry` and resumes loop
  - Dashboard shows HITL input when loop is in `awaiting_human` status
  - Config supports enabling/disabling HITL per transport

## Phase 5: Advanced PR Management (P1)

- [ ] **Phase 5: PR Management — Auto-merge, Dismissal, Inspection** `epic` `p1`
  Auto-merge strategy selection, change-request dismissal, PR inspection commands, log streaming.

  ## Auto-merge Strategy
  Already partially implemented in `reviewer_criteria.rs`. Enhance:
  - Strategy selection: squash, merge, rebase
  - Scope control: all PRs vs spec-prs-only
  - Refusal reasons: disabled, scope, no-branch-protection, strategy-disallowed
  - MergeWatch state machine: Merged/StillPending/RedCI/Conflict/MergeReady/Stuck

  ## Change Request Dismissal
  When reviewer approves but a previous change-request still exists:
  - List reviews for PR
  - Find stale ChangesRequested reviews
  - Dismiss with reason "Stale review dismissed after re-approval"

  ## PR Inspection Commands
  ```bash
  looper pr list [--project <name>] [--status open]
  looper pr show <owner>/<repo>#123
  looper pr status <owner>/<repo>
  looper pr merge <owner>/<repo>#123
  looper pr close <owner>/<repo>#123
  ```

  ## Log Streaming
  ```bash
  looper logs <run-id> --follow
  looper logs <run-id> --follow --filter "error"
  ```
  Implementation: Connect to SSE endpoint, filter by run_id, stream to terminal.

  ## Run Statistics
  ```bash
  looper run-stats [--project <name>] [--period 7d]
  ```
  Aggregate: total runs, success rate, avg/p95 duration, by-role breakdown.

  ## Success Criteria
  - Auto-merge with squash strategy works end-to-end
  - Stale change requests are dismissed after re-approval
  - `looper pr list` shows PRs across all projects
  - `looper logs <id> --follow` streams live output
  - `looper run-stats` shows aggregate statistics

## Phase 6: CLI Polish (P2)

- [ ] **Phase 6: CLI Polish — Hidden Stubs + Ergonomics** `epic` `p2`
  Implement all hidden stub commands, add shell completions, consistent output.

  ## Hidden Stubs to Implement
  - `webhook status|cleanup|rotate|list-orphans|delete`
  - `labels` — Initialize/sync looper labels across projects
  - `prompt` — View/edit per-role agent prompts
  - `feedback` — Submit agent output feedback
  - `netadmin` — Network admin (list nodes, kick, status)

  ## CLI Ergonomics
  - Consistent `--json` output on all commands
  - Colored output via `colored` crate
  - Progress indicators via `indicatif` crate
  - `--verbose` / `-v` flag on all commands
  - Shell completions (bash/zsh/fish) via `clap_complete`
  - Man page generation via `man` crate

  ## Success Criteria
  - All hidden stubs return meaningful output (not "unsupported")
  - `--json` flag works on every command
  - Shell completions generate without errors
  - `looper --help` shows all available commands with descriptions

## Phase 7: CI/CD & Testing (P2)

- [ ] **Phase 7: CI/CD — Sandbox E2E + Coverage** `epic` `p2`
  Parity with Go's CI/CD, comprehensive E2E coverage, sandbox tests.

  ## Sandbox E2E Workflow
  Port from Go's `sandbox-e2e.yml`:
  - Uses dedicated test repo on GitHub
  - Builds release binary
  - Runs looper against sandbox repo
  - 20-minute timeout
  - Uploads artifacts on failure

  ## E2E Test Scenarios
  - Full planner→reviewer→worker→fixer cycle
  - Takeover flow
  - Daemon restart recovery
  - Webhook forwarding
  - Multi-project concurrent
  - Agent vendor switch mid-loop
  - Config hot-reload
  - Dashboard smoke test

  ## CI Enhancements
  - Coverage reporting (cargo-tarpaulin)
  - Property-based testing (proptest) for state machines
  - Fuzzing (cargo-fuzz) for config/completion parsers
  - Benchmarks (criterion) for queue claim, config load

  ## Success Criteria
  - Sandbox E2E workflow runs on push to main
  - All E2E scenarios pass
  - Coverage badge in README
  - No flaky tests in CI

## Phase 8: Extras Beyond Go

- [ ] **Phase 8: Plane Provider + Metrics + Plugins** `epic` `p3`
  Plane provider integration, advanced metrics, plugin system.

  ## Plane Provider
  - REST API client for Plane task management
  - Sync issues from Plane → create loops
  - Config: `[projects.provider] type = "plane"`

  ## Advanced Metrics
  - Prometheus endpoint (`/metrics`)
  - Per-loop, per-role, per-vendor metrics
  - Run duration histogram, success rate gauge

  ## Plugin System
  - `Plugin` trait: on_loop_start, on_loop_complete, on_run_complete, on_error
  - Dynamic loading via `libloading`

  ## Success Criteria
  - Plane issues create looper loops
  - Prometheus metrics scrapeable
  - Plugin trait compiles and can be implemented externally

---

## Treehouse Phase 1

- [ ] **Treehouse Phase 1: Add treehouse-core dependency** `feature` `p1`
  Add treehouse-core as workspace dependency. No behavior change.

  ## Scope
  - Add to workspace Cargo.toml: `treehouse-core = { path = "../treehouse_rust/crates/treehouse-core" }`
  - Add to crates/looper-agent/Cargo.toml
  - Add to crates/looperd/Cargo.toml

  ## Verification
  - `cargo check -p looper-agent` passes
  - `cargo check -p looperd` passes
  - All existing tests pass unchanged

## Treehouse Phase 2

- [ ] **Treehouse Phase 2: LooperEnv + LooperPool** `feature` `p1`
  Implement pool abstraction for worktree management.

  ## Scope
  Create `crates/looper-agent/src/pool.rs`:
  - `LooperEnv` implementing `TreehouseEnv` trait
  - `LooperPool` wrapping `TreehouseCore<LooperEnv>` with acquire/release/gc
  - `PoolConfig` (max_trees, lock_timeout_secs, gc_interval_secs)
  - Config integration: add `pool: Option<PoolConfig>` to Config

  ## Verification
  - Unit tests for LooperEnv and LooperPool
  - `cargo test -p looper-agent` passes

## Treehouse Phase 3

- [ ] **Treehouse Phase 3: Wire into agent executor** `feature` `p1`
  Integrate pool into executor for worktree lifecycle.

  ## Scope
  - `ConfiguredExecutor::start_with_pool()` — acquire worktree before spawn
  - `Execution::run_loop()` — release worktree after agent exits
  - `Execution::kill()` — release worktree after SIGTERM/SIGKILL

  ## Verification
  - E2E: acquire → spawn → agent exits → worktree released
  - E2E: acquire → spawn → kill → worktree released

## Treehouse Phase 4

- [ ] **Treehouse Phase 4: Wire into stop_loop + recovery** `feature` `p1`
  Improve stop semantics and recovery using treehouse.

  ## Scope
  - `RuntimeState` gets per-project pools
  - `stop_loop` kills agent process AND releases worktree from pool
  - Recovery: replace Phase 1 orphan cleanup with `heal_state()` + `pool.gc()`

  ## Verification
  - `looper stop` kills agent AND releases worktree
  - Daemon startup: pool.gc() reclaims orphans

## Treehouse Phase 5

- [ ] **Treehouse Phase 5: Replace cleanup subsystem** `feature` `p1`
  Simplify worktree cleanup using pool.gc().

  ## Scope
  - Simplify `worktree_cleanup/plan.rs` → delegate to pool.gc()
  - Simplify `worktree_cleanup/run.rs` → delegate to pool.gc()
  - Remove `cleanup_stale_worktrees()` from main.rs

  ## Verification
  - No orphan worktrees after daemon restart
  - Graceful shutdown reclaims all worktrees

## Treehouse Phase 6

- [ ] **Treehouse Phase 6: Documentation** `docs` `p2`
  Architecture docs, config reference, migration guide.

  ## Scope
  - Update README.md architecture section
  - Add integration guide to docs/
  - Add doc comments to pool.rs
  - Config reference for [pool] section
