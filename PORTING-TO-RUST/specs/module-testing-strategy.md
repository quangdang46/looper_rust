# Testing Strategy for Looper Rust Port

> Ngôn ngữ: Tiếng Việt (với thuật ngữ kỹ thuật tiếng Anh).
> Mục đích: Đặc tả chiến lược testing toàn diện cho Rust port, dựa trên phân tích Go codebase hiện tại và thiết kế Rust đề xuất.
> **Trung thực**: Không tô hồng — nếu Go code có coverage thấp ở đâu, nói rõ. Nếu Rust testing phức tạp hơn, nói rõ.

---

## 1. Current State of Go Tests

### 1.1 Tổng quan

| Metric | Value |
|--------|-------|
| Total test files | 120 |
| Total test lines | ~81,000 |
| Packages with tests | 42 |
| Largest test file | `internal/api/handler_test.go` (6,682 lines) |
| Smallest test file | `internal/coordinator/mergewatch/mergewatch_test.go` (53 lines) |
| E2E test infrastructure | 7 files, 2,565 lines + harness framework |

### 1.2 Test patterns được sử dụng

- **Table-driven tests**: Pattern phổ biến nhất. Hầu hết logic tests đều dùng `[]struct{name, input, expected}` pattern. Áp dụng cho domain state machine, config validation, queue logic, GitHub parsing.
- **Golden file tests**: CLI output (`cliapp`), config format roundtrip. So sánh output với `.golden` file, hỗ trợ `-update` flag.
- **Mock interfaces**: GitHub gateway, git gateway, storage repositories được mock qua Go interfaces. Dùng `mockgen` hoặc hand-written mocks.
- **Integration tests với real SQLite**: In-memory SQLite (`:memory:`) cho storage tests. Migration runner, repository CRUD, queue operations.
- **Parity tests (JSON fixtures)**: Config parsing tests dùng JSON fixtures để verify config merge behavior. Format roundtrip (TOML -> map -> YAML -> JSON).
- **Real E2E harness**: 5 binaries (looper, looperd, fake-agent, fake-gh, fake-osascript). Temp HOME, temp git repos, dynamic ports, strict fake GitHub CLI.
- **Contract tests**: GitHub CLI command contract verification via fixture-driven schema. Xác thực `--json` field allowlist khớp với thực tế.
- **Sandbox E2E**: Real GitHub sandbox repo cho mutation tests, opt-in via env gate.
- **Property-based tests**: Không có trong Go codebase (Go testing ecosystem không có proptest phổ biến như Rust).

### 1.3 Coverage strengths

1. **Domain state machine**: LoopStatus (10 states, 25 valid transitions) và RunStatus (7 states, 7 valid transitions) được test exhaustively với table-driven patterns.
2. **Config validation**: 300+ validation rules, format roundtrip, env var binding, partial merge. Coverage rất tốt.
3. **Storage layer**: SQLite migration framework, repository CRUD, queue priority ordering, lock acquire/release.
4. **GitHub gateway**: JSON field parsing cho gh CLI output, transient error detection, discovery caching.
5. **API endpoints**: Full HTTP API endpoint contract test (~6,682 lines) covering status codes, envelope format, auth, pagination.
6. **E2E harness**: Daemon boot smoke, worktree isolation, GitHub contract, resolve-comments scenarios, worker no-diff.
7. **Runner logic**: Planner, reviewer, fixer, worker step pipelines được test với mock gateways.

### 1.4 Coverage weaknesses (honest assessment)

1. **CLI golden tests lỏng lẻo**: Nhiều CLI output tests không kiểm tra exact match, chỉ check substring. Một số golden files đã lỗi thời so với implementation.
2. **Reviewer integration test fragile**: Reviewer runner tests dựa vào mocks quá nhiều, không bắt được real logic bugs như checkpoint serialization, resume policy.
3. **Coordinator triage test coverage thấp**: Triage LLM decision logic chỉ được test với hardcoded responses. Không test thực tế decision pipeline variation.
4. **Worker execution isolation test còn thiếu**: Worker worktree isolation được test ở E2E level nhưng không có unit test cho executor process group management.
5. **Network layer test coverage yếu**: Network client/cloud integration tests tồn tại nhưng coverage thấp (785 lines cho 2 packages). Heartbeat, lease, fencing không được test exhaustively.
6. **Webhook event routing test không có**: Không có integration test cho webhook nhận event và route đúng runner lane.
7. **Recovery pipeline test coverage hạn chế**: Startup recovery (orphan cleanup, stale run reconcile) chỉ được test manual, không có automated E2E tests.
8. **Property-based tests không tồn tại**: Không có fuzz/property tests cho state machine, queue operations, config merge — đây là lỗ hổng lớn cho correctness-critical code.

### 1.5 What Go tests are valuable for Rust port

1. **Domain state machine transitions**: Rust port có thể mapping trực tiếp từ Go table-driven tests sang `rstest`.
2. **Config validation rules**: 300+ rules cần được port đầy đủ. Go tests là ground truth.
3. **Runner step definitions và step validation**: Step lists, order invariants là pure logic, dễ port.
4. **GitHub API response JSON parsing**: Go tests define exact JSON shapes. Rust typing sẽ thêm compile-time safety.
5. **SQLite migration test patterns**: Migration sequencing, schema version checks là platform-agnostic.
6. **E2E harness architecture**: FakeGitHub, FakeAgent patterns thiết kế rất tốt, Rust cần implement tương đương.
7. **Critical regression scenarios**: Resolve-comments, worker no-diff, closed-target skip scenarios đã capture real bugs. Rust port phải có equivalent tests.

### 1.6 What Go tests are NOT useful (too coupled to Go idioms)

1. **`interface{}` reflection-heavy patterns**: Go tests dùng `interface{}` và type assertion nhiều cho tham số flexible. Rust's type system sẽ enforce different patterns.
2. **`os/exec` và `syscall` tests**: Go's subprocess management, signal handling, process group management là platform-specific và không trực tiếp port được.
3. **`context.Context` propagation tests**: Go context cancel/deadline patterns khác với Rust's async cancellation (`tokio::select!`, `CancellationToken`).
4. **`testing.TB` helper patterns**: Go's `t.Helper()`, `t.Cleanup()`, `t.Fatalf()` patterns không có direct Rust equivalent. Rust test harness sẽ khác.
5. **`json.RawMessage` và dynamic JSON parsing tests**: Go gateway tests dùng `map[string]any` cho flexible JSON parsing. Rust dùng `serde_json::Value` tương đương nhưng pattern khác.
6. **Interface mock generation**: Go's `mockgen` + `gomock` patterns. Rust có `mockall` nhưng pattern khác biệt đáng kể.
7. **Go test binary compilation**: Go compile test binaries trực tiếp. Rust có proc-macro overhead longer compile times.

---

## 2. E2E Harness Deep Dive

### 2.1 FakeGitHub

File: `internal/e2e/harness/cmd/fake-gh/main.go` (~630 lines)

FakeGitHub là standalone Go binary được build từ `TestMain`. Nó intercept `gh` CLI calls bằng cách set `tools.ghPath` trong config.

**Kiến trúc**:

- **Environment-based configuration**:
  - `LOOPER_E2E_FAKE_GH_MODE`: `"strict"` (default), `"record"`, `"replay"`
  - `LOOPER_E2E_FAKE_GH_STATE_PATH`: JSON state file path (cross-process persistence)
  - `LOOPER_E2E_FAKE_GH_GIT_PATH`: git binary path for head SHA resolution
  - `LOOPER_E2E_FAKE_GH_ARTIFACT_DIR`: output directory
  - `LOOPER_E2E_FAKE_GH_SCHEMA_PATH`: field allowlist schema path
  - `LOOPER_E2E_FAKE_GH_RECORD_PATH`: recording output path

- **Core routing**: Parse `os.Args[1:]` để xác định command key (vd: `"pr list"`, `"pr view"`, `"api"`). Dispatch dựa trên prefix matching.
  - `Commands` map: exact argv match -> override response
  - `"pr list"`, `"issue list"`: emit default JSON với allowed fields
  - `"pr view"`: lookup PR state từ state file, resolve SHA từ git bare repo
  - `"pr merge"`: mutate state (MERGED, close linked issues)
  - `"api"`: route REST API endpoints (issues, pulls, reviews, check runs, graphql)
  - GraphQL: resolveReviewThread, unresolveReviewThread, addPullRequestReviewThreadReply, review threads query

- **State management**: 
  - `GHState` struct gồm `Commands`, `Routes`, `GraphQL`, `CurrentUserLogin`, `PullRequests`
  - State file đọc/ghi qua JSON serialization
  - `PullRequests` map key: `"{repo}#{prNumber}"` (vd: `"acme/looper#42"`)
  - Hydration: resolve SHA từ bare git repo qua `rev-parse`
  - Normalization: fill defaults (author=octocat, mergeStateStatus=CLEAN)

- **Validation**: 
  - Schema-driven JSON field allowlist per command
  - `validateFields(command, requestedFields, allowed)`: reject unsupported --json fields
  - Phân biệt summary vs detail fields: PR list fields != PR view fields

**Endpoints mocked**:

| gh command | HTTP equivalent | State needed |
|-----------|----------------|--------------|
| `gh pr list --json ...` | GraphQL search | Schema allowlist |
| `gh pr view <n> --json ...` | REST pulls endpoint | PullRequests map |
| `gh pr merge <n> --auto` | REST merge | PullRequests, Route |
| `gh issue list --json ...` | REST issues | Schema allowlist |
| `gh issue view <n>` | REST issues | Route map |
| `gh api repos/<r>/issues/<n>` | REST issues/PRs | Route/PullRequests map |
| `gh api graphql` | GraphQL | GraphQL map |
| `gh auth status` | N/A | CurrentUserLogin |
| `gh label list` | REST labels | Schema allowlist |

**Limitations**:
- Không support real pagination beyond `--paginate` returning empty array
- Không simulate race conditions (concurrent PR state changes)
- Không simulate rate limiting (HTTP 429)
- Schema không được refresh tự động từ real gh

### 2.2 FakeAgent

File: `internal/e2e/harness/cmd/fake-agent/main.go` (~340 lines)

FakeAgent là standalone Go binary simulating AI coding CLI (Claude Code, Codex, etc.).

**Modes supported**:

| Mode | Behavior |
|------|----------|
| `success-with-diff` | Write file + completion marker |
| `success-no-diff` | Completion marker only (no file changes) |
| `write-file` | Write file at configurable path |
| `modify-file` | Append to existing file |
| `commit` | Git add + commit + completion marker with review replies |
| `commit-with-review-replies` | Commit + fetch thread hash + include reply data |
| `transient-failure` | First run exits 1, second run succeeds |
| `malformed-marker` | Output completion marker with bad JSON |
| `timeout` / `no-marker` | Output without completion marker, exit cleanly |

**Evidence mechanism**: Mỗi lần chạy ghi `cwd-evidence.json` với:
- `cwd`: actual working directory
- `args`: command arguments
- `env`: key environment variables (masked)
- `timestamp`: ISO 8601
- `mode`: agent mode
- `pid`: process ID

**Review thread reply**: FakeAgent có thể parse prompt để extract fix items, fetch review thread state via gh, và build review thread replies. Điều này cho phép simulate realistic fixer behavior.

### 2.3 TestSetup / Harness infrastructure

**Binary compilation** (`binaries.go`):
- `TestMain` gọi `RunTestMain` -> `buildAll()` -> `go build` cho 5 binaries
- Binaries được build vào temp directory
- `MustBinaries(tb)` cache kết quả (compile once per package)
- Hỗ trợ pre-built binaries qua env vars

**TempHome** (`temp_home.go`):
```go
type TempHome struct {
    Root         string  // temp directory root
    HomeDir      string  // ~/ (HOME env)
    LooperHome   string  // ~/.looper/
    LogDir       string  // ~/.looper/logs/
    BackupDir    string  // ~/.looper/backups/
    WorktreeRoot string  // ~/.looper/worktrees/
    WorkingDir   string  // working directory
    DBPath       string  // ~/.looper/looper.sqlite
    ConfigPath   string  // ~/.looper/config.json
    ArtifactsDir string  // test artifacts
}
```

**Config builder** (`config.go`):
```go
type ConfigOptions struct {
    Port              int
    WorkingDir        string
    ToolPaths         TestToolPaths
    EnableOsascript   bool
    AgentVendor       *config.AgentVendor
    AgentCommand      string
    AgentEnv          map[string]string
    Projects          []config.ProjectRefConfig
    DisableDisclosure bool
}
```

- `DefaultConfig`: tạo config từ options, set tool paths, vendor, env
- `WriteConfig`: marshal config, apply raw JSON overrides (cho legacy field simulation)

**Daemon management** (`daemon.go`):
- `StartLooperd`: start looperd binary với stdout/stderr logging
- `WaitForReady`: HTTP poll `/api/v1/status` với timeout
- `Stop`: SIGINT -> 5s grace -> SIGKILL
- `DumpArtifacts`: output config, stdout, stderr, log dir, artifact dir on failure

**Git helpers** (`git.go`):
- `CreateSeededRepo`: git init, initial commit, config user
- `CreateBareOrigin`: git clone --bare, add remote, push
- `CreateBranchCommitAndPush`: create branch, commit, push
- `SnapshotRepo`: capture HEAD, status porcelain, index tree, worktree list
- `AssertRepoUnchanged`: verify snapshot invariants

**Assertions** (`assertions.go`):
- `AssertRepoUnchanged`: head + status + index tree unchanged
- `AssertCWDInsideWorktree`: cwd path prefix check
- `AssertCWDNotRepoPath`: cwd != repo path

### 2.4 Contract tests

File: `internal/e2e/githubcontract/contract_test.go` (~321 lines)

Contract tests verify:
1. `ListOpenIssues` requests correct JSON fields từ gh CLI
2. `ListOpenPullRequests` requests correct fields (không có `authorAssociation`)
3. `ViewIssue` requests detail-level fields (có `authorAssociation`, `stateReason`)
4. ResolveReviewThread fires GraphQL mutation
5. Dependency wrappers (`ListBlockedByIssues`, `ListBlockingIssues`, `ListSubIssues`) use correct API routes
6. Repo forms (owner/repo, github.com/owner/repo, ghe.example.com/owner/repo) handled correctly
7. Unsupported JSON fields cause fake-gh to exit non-zero

Assertion pattern:
```go
assertInvocationHasJSONFields(t, invocations, "issue", "list", []string{"number", "title", "body"})
assertInvocationMissingJSONField(t, invocations, "issue", "list", "authorAssociation")
```

### 2.5 Scenario-based tests

File: `internal/e2e/resolve_comments_scenarios_test.go` (~630 lines)

Các scenarios test end-to-end dengan real daemon + fake GitHub + fake agent:

1. **Stale-head-after-push**: Fixer tạo new commit, push, fake-gh PR head SHA thay đổi -> fixer phải refresh head
2. **Multi-comment thread**: Thread với 2 comments, cả 2 được resolved
3. **Closed PR skip**: PR state = CLOSED -> skip, không chạy agent
4. **No-new-commit unresolved**: Agent returns no diff -> push skipped -> thread unresolved -> run failed với `restart_from_discover` resume policy
5. **Stale no-push metadata**: Rerun với stale head SHA + same fix items hash -> detect no change -> skip
6. **Worker no-diff no PR**: Agent creates no diff -> compare branches -> skip PR creation
7. **Resumed fixer stops on closed PR**: Paused loop với run failed -> resume -> PR closed -> skip

Pattern chung:
```go
1. CreateSeededRepo + CreateBareOrigin
2. CreateBranchCommitAndPush (seed feature branch)
3. FakeGH.WriteState (seed PR + review threads, linked to bare origin)
4. Write config with fake tools
5. StartLooperd -> WaitForReady
6. Create loop via API -> WaitForRunTerminal
7. Assertions:
   - Checkpoint fields (push, resolvedComments, skipReason)
   - FakeGH state (thread resolved, PR state)
   - Invocation log (gh CLI calls)
   - Agent evidence (cwd, not executed)
8. Stop looperd
```

---

## 3. Proposed Rust Testing Architecture

### Overview

Rust testing architecture gồm 5 layers, thiết kế để tận dụng Rust's type system và testing ecosystem.

```
Layer 1: Unit Tests (pure logic, no IO)
Layer 2: Integration Tests (cross-crate, in-memory SQLite, mock services)
Layer 3: E2E Tests (binary-level, fake servers)
Layer 4: Snapshot Tests (golden files with insta)
Layer 5: Property/Fuzz Tests (proptest, cargo-fuzz)
```

### 3.1 Layer 1 — Unit Tests (8,000-12,000 lines)

Mỗi crate có test module riêng trong `src/` (unit tests) và option `tests/` (integration tests).

#### looper-core

| Test area | Frameworks | Est. lines | Go mapping |
|-----------|-----------|-----------|------------|
| LoopStatus state machine (10 states, 25/65 valid/invalid transitions) | `#[test]`, `rstest` | 800 | `domain_test.go` |
| RunStatus state machine (7 states, 7/42 valid/invalid transitions) | `#[test]`, `rstest` | 400 | `domain_test.go` |
| Status classification (IsActive, IsConflicting, IsTerminal) | `#[test]` | 200 | `domain_test.go` |
| LoopTargetKey construction (project/issue/PR formats) | `#[test]` | 100 | `domain_test.go` |
| AssertLoopTypeMatchesTarget (4 loop types x 3 target types) | `rstest` | 150 | `domain_test.go` |
| AssertUniqueActiveLoop conflict detection | `rstest` | 300 | `domain_test.go` |
| AssertStepBelongsToLoopType (all 4 step lists) | `rstest` | 200 | `domain_test.go` |
| ResumePolicy normalization + default selection | `rstest` | 400 | `lifecycle_test.go` |
| SuppressesAutonomousRecovery + ShouldRestartFromDiscover | `rstest` | 200 | `lifecycle_test.go` |
| Failure classification (boundary-based) | `rstest` | 400 | `failureclass_test.go` |
| **Subtotal** | | **~3,150** | |

**Tổ chức**:
- `src/domain/status.rs` có `#[cfg(test)] mod tests` với helper functions
- `src/domain/resume.rs` có test module riêng
- Dùng `rstest` cho parameterized tests thay vì Go table-driven pattern

**Go vs Rust khác biệt**:
- Go dùng `map[LoopStatus][]LoopStatus` cho transition matrix. Rust dùng `match` hoặc `EnumMap` với type safety.
- Go's `error` vs Rust's `Result<(), ValidationError>`. Rust enum variants cho phép structured errors.
- Rust's `#[non_exhaustive]` cho status enums prevents pattern match breakage.

#### looper-core-config

| Test area | Frameworks | Est. lines | Go mapping |
|-----------|-----------|-----------|------------|
| 300+ validation rules (port range, auth modes, intervals) | `#[test]`, `rstest` | 1,200 | `config_test.go` |
| Partial merge from 3 layers (defaults -> file -> env -> CLI) | `rstest` | 600 | `config_test.go`, `config_parity_test.go` |
| Format roundtrip (TOML serialization and back) | `#[test]`, `insta` | 400 | `format_roundtrip_test.go` |
| Env var binding for 30+ env vars | `rstest` | 300 | `config_parity_test.go` |
| Deprecated env/CLI warning detection | `#[test]` | 200 | `config_test.go` |
| Protected instruction phrase blocking (20+ phrases) | `rstest` | 200 | N/A (Go doesn't have this) |
| **Subtotal** | | **~2,900** | |

**Tổ chức**:
- `src/config/validation.rs` — unit tests per validation function
- `src/config/merge.rs` — merge order + precedence tests
- `src/config/format.rs` — serialization roundtrip
- `src/tests/config/` — integration-style tests (but still pure logic, no IO)

**Lưu ý cho Rust**: Config validation trong Rust phức tạp hơn Go vì Rust's type system yêu cầu proper error types. Không thể dùng `map[string]any` để bypass type checks.

#### looper-core-disclosure

| Test area | Frameworks | Est. lines | Go mapping |
|-----------|-----------|-----------|------------|
| Disclosure stamp construction per channel | `rstest` | 200 | `disclosure_test.go` |
| Disclosure stamp stripping and detection | `rstest` | 150 | `disclosure_test.go` |
| Diffanchor parse(Index) from diff text | `rstest` | 250 | `diffanchor_test.go` |
| Diffanchor validate() for valid/invalid anchors | `rstest` | 200 | `diffanchor_test.go` |
| **Subtotal** | | **~800** | |

#### looper-storage

| Test area | Frameworks | Est. lines | Go mapping |
|-----------|-----------|-----------|------------|
| SQLite migration framework (run all, verify schema version) | `#[test]`, `tempfile` | 400 | `migrate_test.go` |
| Repository methods: queues, loops, runs, projects, worktrees | `rstest`, `tempfile` | 1,200 | `repositories_test.go` |
| Queue priority ordering | `rstest` | 200 | `repositories_test.go` |
| Lock acquire/release/expiry with TTL | `#[test]` | 200 | `sqlite_driver_test.go` |
| Unique constraint + upsert (CreateOrGetActiveByDedupe) | `rstest` | 200 | `repositories_test.go` |
| Seq counter atomic increment | `#[test]` | 100 | N/A (Rust-specific) |
| OneRunningRunPerLoop constraint | `rstest` | 100 | `repositories_test.go` |
| **Subtotal** | | **~2,400** | |

**Tổ chức**:
- `src/storage/migrations/` — migration test helper
- `src/tests/` — integration tests with in-memory SQLite
- Dùng `tempfile::TempDir` cho database file, không dùng `:memory:` để tránh concurrency issues

**Go vs Rust khác biệt**:
- Go dùng `database/sql` interface (runtime polymorphism). Rust dùng `rusqlite` với compile-time queries.
- Go's `sql.NullString` pattern vs Rust's `Option<String>` via `rusqlite`'s `from_sql`.
- Rust ownership: connection pool management cần `Arc<Mutex<Connection>>` hoặc `r2d2` pool.

#### looper-github

| Test area | Frameworks | Est. lines | Go mapping |
|-----------|-----------|-----------|------------|
| JSON response parsing for all gh CLI outputs | `rstest`, `serde_test` | 400 | `gateway_test.go` |
| Gateway helpers (parse_repo, extract_author, etc.) | `#[test]` | 200 | `gateway_test.go` |
| Review idempotency marker + submission | `rstest` | 300 | `review_anchor_test.go` |
| Discovery cache TTL invalidation | `#[test]` | 200 | `discovery_snapshot_test.go` |
| Transient error pattern matching (20+ patterns) | `rstest` | 200 | `errors.go` tests |
| Label color normalization | `#[test]` | 100 | N/A (no Go unit test) |
| **Subtotal** | | **~1,400** | |

#### looper-git

| Test area | Frameworks | Est. lines | Go mapping |
|-----------|-----------|-----------|------------|
| buildWorktreeDirectoryName for various inputs | `rstest` | 100 | `gateway_test.go` |
| sanitizeBranchName with special characters | `rstest` | 80 | `gateway_test.go` |
| AssertWritableBranch with protected branch list | `rstest` | 100 | `gateway_test.go` |
| pushConflictErrorPattern regex matching | `rstest` | 50 | `gateway_test.go` |
| Unsafe git env var filtering (16 keys) | `#[test]` | 80 | `gateway_test.go` |
| **Subtotal** | | **~410** | |

#### looper-agent

| Test area | Frameworks | Est. lines | Go mapping |
|-----------|-----------|-----------|------------|
| resolveCommand for all 5 vendors | `rstest` | 300 | `executor_test.go` |
| Args construction: fresh vs native resume variants | `rstest` | 500 | `executor_test.go` |
| Model flag positioning | `rstest` | 200 | `executor_test.go` |
| Prompt flag handling (--print, -z, inline, stdin) | `rstest` | 200 | `executor_test.go` |
| Native session ID extraction from stdout/stderr | `rstest` | 200 | `executor_test.go` |
| Environment variable construction (unsafe git keys, LOOPER_PROMPT) | `rstest` | 200 | `executor_test.go` |
| Completion marker parsing | `rstest` | 200 | `executor_test.go` |
| finalStatus determination | `rstest` | 150 | `executor_test.go` |
| **Subtotal** | | **~1,950** | |

**Tổ chức**:
- Tests structured per AgentVendor để dễ maintain khi thêm vendor mới
- Dùng `rstest` case names: `codex_fresh_spawn`, `codex_native_resume`, etc.
- Environment construction tests verify exact key order + unsaved git vars stripped

#### looper-core-network-policy

| Test area | Frameworks | Est. lines | Go mapping |
|-----------|-----------|-----------|------------|
| EvaluateWorker/Reviewer claim decisions (off vs routed) | `rstest` | 150 | `policy_test.go` |
| ParseTargetLabel, CollectTargetLabels, HasExactTarget | `#[test]` | 100 | `policy_test.go` |
| matchLocalIdentity (numeric match, login, priority) | `rstest` | 100 | `policy_test.go` |
| **Subtotal** | | **~350** | |

**Tổng Layer 1**: ~13,360 lines (có thể giảm nếu consolidate tests)

### 3.2 Layer 2 — Integration Tests (6,000-10,000 lines)

Khác với unit tests, integration tests chạy cross-crate với in-memory SQLite, mock gateways, và async runtime.

#### Service layer

| Test area | Frameworks | Est. lines | Go mapping |
|-----------|-----------|-----------|------------|
| LoopService.Create: validation, conflict detection, seq | `#[tokio::test]`, `mockall` | 500 | `loops/service_test.go` |
| LoopService.TransitionStatus: all valid/invalid transitions | `rstest`, `#[tokio::test]` | 400 | `loops/service_test.go` |
| LoopService.Pause/Terminate/Resume | `#[tokio::test]`, `mockall` | 400 | `loops/service_test.go` |
| RunService.Start: one-running-run, step validation | `#[tokio::test]`, `mockall` | 400 | `runs/service_test.go` |
| RunService.RecordStep/Complete | `#[tokio::test]`, `mockall` | 300 | `runs/service_test.go` |
| ProjectService.Add: ID collision, repo auto-detect, worktree | `#[tokio::test]`, `mockall` | 500 | `projects/service_test.go` |
| ProjectService.Remove: archive, terminate loops, cleanup | `#[tokio::test]`, `mockall` | 300 | `projects/service_test.go` |
| ProjectService.SyncConfigured: upsert, non-destructive | `#[tokio::test]`, `mockall` | 200 | `projects/service_test.go` |
| **Subtotal** | | **~3,000** | |

**Tổ chức**:
- Mỗi service có integration test file riêng trong `tests/` directory của crate tương ứng
- Hoặc centralized trong `tests/integration/` workspace-level test suite
- Mock gateways qua traits: `GitHubGateway`, `GitGateway`, `Storage`

#### Scheduler and runners

| Test area | Frameworks | Est. lines | Go mapping |
|-----------|-----------|-----------|------------|
| Scheduler tick cycle (pre-claim, discovery, claim, slot calc) | `#[tokio::test]`, `mockall` | 500 | `scheduler_test.go` |
| Claim priority ordering (non-retry first, then retry) | `rstest`, `#[tokio::test]` | 300 | `scheduler_test.go` |
| Webhook event routing to correct runner lane | `#[tokio::test]`, `mockall` | 400 | N/A (Go không có) |
| Planner runner 5-step pipeline | `#[tokio::test]`, `mockall` | 500 | `planner/runner_test.go` |
| Reviewer runner 6-step pipeline | `#[tokio::test]`, `mockall` | 600 | `reviewer/runner_integration_test.go` |
| Fixer runner 10-step pipeline | `#[tokio::test]`, `mockall` | 700 | `fixer/runner_test.go` |
| Worker runner 6-step pipeline | `#[tokio::test]`, `mockall` | 500 | `worker/runner_test.go` |
| Runner error classification + retry behavior | `rstest` | 400 | `failureclass_test.go` |
| Resume policy adherence on re-execution | `rstest`, `#[tokio::test]` | 300 | `runner-retry-recovery spec` |
| **Subtotal** | | **~4,200** | |

#### Coordinator submodules

| Test area | Frameworks | Est. lines | Go mapping |
|-----------|-----------|-----------|------------|
| Triage Decide: valid/out-of-scope/unclear dispositions | `rstest`, `mockall` | 400 | `triage/triage_test.go` |
| Dispatch: human-gated (slash command parsing) | `rstest` | 300 | `dispatch/dispatch_test.go` |
| Dispatch: autonomous (hold label veto, delay gate) | `rstest` | 250 | `dispatch/dispatch_test.go` |
| DependencyGraph.build: ready_set, blocker state, cycles | `rstest` | 350 | `depgraph/depgraph_test.go` |
| MergeWatch.Classify: all 8 action kinds | `rstest` | 350 | `mergewatch/mergewatch_test.go` |
| **Subtotal** | | **~1,650** | |

#### Runtime and recovery

| Test area | Frameworks | Est. lines | Go mapping |
|-----------|-----------|-----------|------------|
| Daemon bootstrap: config load, tool validation, logger | `#[tokio::test]`, `tempfile` | 400 | `runtime_test.go` |
| CompleteStartup pipeline: recovery phases | `#[tokio::test]`, `mockall` | 500 | `runtime_test.go` |
| Stale run reconciliation (startup vs live mode) | `rstest`, `#[tokio::test]` | 400 | `runtime_test.go` |
| Orphan agent cleanup: PID matching, SIGTERM/SIGKILL | `#[tokio::test]`, `mockall` | 300 | `runtime_test.go` |
| Expired lock release | `#[tokio::test]` | 200 | `runtime_test.go` |
| Loop normalization for manual_intervention | `#[tokio::test]` | 200 | `runtime_test.go` |
| Worktree cleanup Plan(): cross-reference logic | `rstest`, `#[tokio::test]` | 400 | `worktreecleanup/service_test.go` |
| Worktree cleanup Run(): safety, dirty protection | `#[tokio::test]`, `tempfile` | 300 | `worktreecleanup/service_test.go` |
| **Subtotal** | | **~2,700** | |

#### Network layer

| Test area | Frameworks | Est. lines | Go mapping |
|-----------|-----------|-----------|------------|
| Client join/heartbeat/leave lifecycle | `#[tokio::test]`, `wiremock` | 400 | `network/client/*` |
| Coordinator lease acquire/renew/expire/handoff | `#[tokio::test]` | 400 | `network/client/*` |
| Identity drift detection | `#[tokio::test]` | 150 | N/A |
| Event subscription via SSE | `#[tokio::test]`, `wiremock` | 200 | N/A |
| **Subtotal** | | **~1,150** | |

#### API layer

| Test area | Frameworks | Est. lines | Go mapping |
|-----------|-----------|-----------|------------|
| Route handlers status codes + envelope format | `#[tokio::test]`, `axum::test` | 800 | `api/handler_test.go` |
| Auth middleware (no auth, Bearer, misconfigured) | `#[tokio::test]`, `axum::test` | 300 | `api/handler_test.go` |
| Endpoint pagination + filtering | `rstest`, `#[tokio::test]` | 300 | `api/handler_test.go` |
| Error response codes per error type | `rstest` | 200 | `api/handler_test.go` |
| SSE log streaming | `#[tokio::test]` | 150 | N/A (Go API test) |
| Health/version endpoints | `#[tokio::test]` | 100 | `api/handler_test.go` |
| **Subtotal** | | **~1,850** | |

**Tổng Layer 2**: ~14,550 lines (ước lượng thấp hơn nếu mock infrastructure được dùng chung)

### 3.3 Layer 3 — E2E Tests (3,000-5,000 lines)

Binary-level tests với fake servers, tương đương Go E2E harness.

#### FakeGitHub (Rust)

Thiết kế khác với Go version — sử dụng axum HTTP server thay vì standalone binary mimics `gh` CLI.

```rust
// Chạy như HTTP server
struct FakeGitHubServer {
    addr: SocketAddr,
    state: Arc<RwLock<FakeGitHubState>>,
    shutdown: tokio::sync::oneshot::Sender<()>,
}

// Endpoints
// GET /repos/{owner}/{repo}/pulls — list PRs
// GET /repos/{owner}/{repo}/pulls/{number} — PR detail
// GET /repos/{owner}/{repo}/issues — list issues
// GET /repos/{owner}/{repo}/issues/{number} — issue detail
// POST /repos/{owner}/{repo}/pulls/{number}/reviews — submit review
// GET /repos/{owner}/{repo}/pulls/{number}/reviews — list reviews
// POST /repos/{owner}/{repo}/pulls/{number}/reviews/{id}/comments
// PATCH /repos/{owner}/{repo}/pulls/{number} — update PR
// GraphQL endpoints via POST /graphql
// POST /repos/{owner}/{repo}/labels — create label
// GET /repos/{owner}/{repo}/labels — list labels
```

State management patterns:
```rust
struct FakeGitHubState {
    pull_requests: HashMap<String, PullRequest>,      // key: "owner/repo#number"
    issues: HashMap<String, Issue>,
    review_threads: HashMap<String, ReviewThread>,
    comments: HashMap<String, Comment>,
    current_user: String,
    labels: HashMap<String, LabelDefinition>,
}
```

FakeGitHub không cần mirror `gh` CLI command parsing (như Go version) vì Rust port dùng `octocrab` crate thay vì `exec.Command("gh", ...)`. Rust FakeGitHub mô phỏng REST API endpoints mà `octocrab` gọi.

**Ưu điểm so với Go version**:
- Loại bỏ complex `gh` CLI argument parsing (không cần `--json`, `--jq`, field allowlist)
- API response types được define bằng `serde` structs, type-safe
- Dễ maintain hơn khi routes tương ứng trực tiếp với octocrab calls
- Không cần shell command interposition

**Nhược điểm so với Go version**:
- Không test được actual `gh` CLI compatibility (contract testing cần cách khác)
- `octocrab` client implementation cần test riêng vs real GitHub API

**Implementation notes**:
- State persistence: không cần cross-process JSON file (Rust process test)
- `Arc<RwLock<...>>` cho shared state
- Axum router với typed extractors
- Configurable delay simulation (network latency, timeout)
- Rate limiting simulation

#### FakeAgent (Rust)

Rust E2E fake agent khác với Go version. Thay vì standalone binary, Rust E2E tests spawn agent executor trực tiếp trong process với mock I/O.

```rust
struct FakeAgent {
    mode: AgentMode,
    artifact_dir: PathBuf,
    completion_marker: String,
}

enum AgentMode {
    SuccessWithDiff { files: Vec<PathBuf> },
    SuccessNoDiff,
    TransientFailure,
    MalformedMarker,
    Timeout,
    Commit { files: Vec<PathBuf>, commit_message: String },
    CommitWithReviewReplies { 
        files: Vec<PathBuf>,
        review_threads: Vec<ReviewThreadReply>,
    },
}
```

**Tích hợp với runner pipeline**:
Implement `AgentExecutor` trait với mock behavior:
```rust
#[async_trait]
pub trait AgentExecutor: Send + Sync {
    async fn execute(&self, input: RunInput) -> Result<AgentResult>;
}
```

FakeAgent implementation:
```rust
struct MockAgentExecutor {
    mode: AgentMode,
    evidence: Arc<Mutex<Vec<AgentEvidence>>>,
}

#[async_trait]
impl AgentExecutor for MockAgentExecutor {
    async fn execute(&self, input: RunInput) -> Result<AgentResult> {
        // Record evidence
        self.evidence.lock().push(AgentEvidence {
            cwd: input.working_directory.clone(),
            prompt: input.prompt.clone(),
            mode: format!("{:?}", self.mode),
        });
        
        // Return result based on mode
        match &self.mode {
            AgentMode::SuccessNoDiff => Ok(AgentResult {
                status: "completed".into(),
                summary: "agent completed".into(),
                changed_files: vec![],
                commits: vec![],
                ..Default::default()
            }),
            AgentMode::Timeout => Err(AgentError::Timeout {
                elapsed: Duration::from_secs(60),
                timeout_type: "idle".into(),
            }),
            // ...
        }
    }
}
```

**Evidence pattern**: FakeAgent ghi `cwd-evidence.json` để test có thể verify working directory isolation:
```rust
struct AgentEvidence {
    cwd: PathBuf,
    prompt: String,
    mode: String,
    timestamp: DateTime<Utc>,
}
```

#### Test infrastructure cho E2E

**TempHome** (Rust equivalent):
```rust
struct TempHome {
    root: TempDir,
    home_dir: PathBuf,
    looper_home: PathBuf,
    log_dir: PathBuf,
    worktree_root: PathBuf,
    working_dir: PathBuf,
    db_path: PathBuf,
    config_path: PathBuf,
}

impl TempHome {
    fn new() -> Self { /* create temp directories */ }
    fn env_map(&self) -> HashMap<String, String> { /* HOME mapping */ }
}
```

**Git helpers** (Rust equivalent, dùng `git2` crate):
```rust
struct SeededRepo {
    path: PathBuf,
    default_branch: String,
    initial_commit: String,
}

fn create_seeded_repo(git_path: &Path) -> Result<SeededRepo>;
fn create_bare_origin(repo_path: &Path) -> Result<PathBuf>;
fn create_branch_commit_and_push(
    repo_path: &Path, branch: &str, file: &str, content: &str
) -> Result<String>;
fn snapshot_repo(repo_path: &Path) -> Result<RepoSnapshot>;

struct RepoSnapshot {
    head: String,
    status_porcelain: String,
}
```

**Daemon management**:
```rust
struct DaemonProcess {
    child: Child,
    base_url: String,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

impl DaemonProcess {
    async fn start(config_path: &Path, extra_env: HashMap<String, String>) -> Result<Self>;
    async fn wait_for_ready(&self, timeout: Duration) -> Result<()>;
    async fn stop(&self) -> Result<()>;
}
```

**Giải pháp cho `looperd` binary test**: Tránh compile test binary mỗi lần chạy E2E test.
- Pre-build `looperd` binary cho tests
- Dùng `cargo build --package looperd` trong build script
- Test phát hiện binary path qua env var: `LOOPER_E2E_LOOPERD_PATH`
- Fallback: compile tại chỗ với `build.rs` hoặc workspace-level harness

#### Test scenarios (Rust E2E)

Danh sách scenarios tối thiểu cho Rust port:

1. **Daemon boot smoke**: Default config, roles config, explicit tool paths, unknown fields
2. **Planner full cycle**: Labeled issue -> planner enqueues -> agent executes -> spec PR created
3. **Reviewer full cycle**: Draft PR -> reviewer enqueues -> agent reviews -> review submitted
4. **Fixer full cycle**: PR with review comments -> fixer enqueues -> agent repairs -> fix pushed -> threads resolved
5. **Worker full cycle**: Labeled issue -> worker enqueues -> agent implements -> PR created
6. **Worktree isolation**: Agent cwd is inside worktree, not user repo
7. **Coordinator triage + dispatch**: Untriaged issue -> triage -> human dispatches via `/plan`
8. **Resolve-comments stale head**: Push changes PR head, fixer must refresh
9. **Closed target skip**: PR closed mid-run -> fixer skips
10. **Worker no-diff no PR**: Agent creates no diff -> skip PR creation
11. **Recovery after crash**: Kill daemon, restart, verify recovery pipeline
12. **Stale run reconcile**: Simulate stuck running run, trigger reconcile
13. **Network join/leave**: Mock loopernet server
14. **Config migration**: JSON to TOML, deprecated fields

### 3.4 Layer 4 — Snapshot Tests (1,000-2,000 lines)

Dùng `insta` crate cho golden/snapshot testing.

**CLI output snapshots**:
- Help text cho tất cả commands và subcommands
- `--json` output shapes
- Error message formats cho mọi failure modes
- `looper ps` output formats (table, json)

**Config snapshots**:
- Default config in TOML/YAML/JSON
- Each section serialized separately
- Validation error messages
- Partial config merge snapshots

**API response snapshots**:
- JSON envelope format consistency
- Error codes per type
- Health/status/version shapes

**Protocol message snapshots**:
- All network message types (JoinRequest, HeartbeatRequest, etc.)

**Implementation**:
```rust
// tests/snapshots/config_tests.rs
#[test]
fn default_config_toml_snapshot() {
    let cfg = Config::default();
    let toml = toml::to_string(&cfg).unwrap();
    insta::assert_snapshot!(toml);
}

#[test]
fn error_messages_snapshots() {
    let err = ConfigValidationError::InvalidPort { port: 99999 };
    insta::assert_display_snapshot!(err);
}
```

**Snapshot management**:
- Dùng `cargo-insta` CLI tool
- CI chạy `cargo insta test --review` hoặc `--accept` để update
- Snapshot files committed vào repo trong `tests/snapshots/`

### 3.5 Layer 5 — Property/Fuzz Tests (1,000-2,000 lines)

Dùng `proptest` cho property-based testing.

**State machine invariants**:
```rust
proptest! {
    #[test]
    fn loop_status_transitions_never_panic(
        from in prop_oneof![
            LoopStatus::Idle, LoopStatus::Queued, LoopStatus::Running,
            LoopStatus::Paused, LoopStatus::Waiting,
            LoopStatus::Stopped, LoopStatus::Terminated,
            LoopStatus::Completed, LoopStatus::Failed, LoopStatus::Interrupted,
        ],
        to in prop_oneof![
            LoopStatus::Idle, LoopStatus::Queued, LoopStatus::Running,
            LoopStatus::Paused, LoopStatus::Waiting,
            LoopStatus::Stopped, LoopStatus::Terminated,
            LoopStatus::Completed, LoopStatus::Failed, LoopStatus::Interrupted,
        ],
    ) {
        let result = assert_loop_status_transition(from, to);
        // If transition exists, it must succeed
        if EXPECTED_TRANSITIONS.contains(&(from, to)) {
            prop_assert!(result.is_ok());
        } else {
            prop_assert!(result.is_err());
        }
    }
}
```

**Queue priority invariants**:
```rust
proptest! {
    #[test]
    fn queue_priority_ordering(
        items in prop::collection::vec(queue_item_strategy(), 1..100),
    ) {
        let queue = Queue::from(items);
        let claimed = queue.claim_all();
        // Items must be claimed in priority order
        for window in claimed.windows(2) {
            prop_assert!(window[0].priority <= window[1].priority);
        }
    }
}
```

**Config merge invariants**:
```rust
proptest! {
    #[test]
    fn config_merge_idempotent(
        base in config_strategy(),
        override1 in partial_config_strategy(),
        override2 in partial_config_strategy(),
    ) {
        let merged = base.clone().merge(override1).merge(override2);
        // Explicit fields from later overrides must win
        // All None values must be resolved
    }
}
```

**Coordinator depgraph invariants**:
```rust
proptest! {
    #[test]
    fn depgraph_no_cycles(
        issues in prop::collection::vec(issue_strategy(), 1..20),
        dependencies in prop::collection::vec(dependency_strategy(), 0..50),
    ) {
        let graph = DependencyGraph::build(&issues, &dependencies);
        // After cycle removal, no cycles remain
        prop_assert!(graph.detect_cycles().is_empty());
    }
}
```

**Fuzz targets**: Dùng `cargo-fuzz` với AFL/LibFuzzer:
- Config parser: random bytes -> `toml::from_slice`
- Diffanchor parser: random diff text -> `parse()` no panic
- JSON response parser: random bytes -> `serde_json::from_slice`

---

## 4. Specific Critical Test Scenarios

### 4.1 Domain state machine transitions (EVERY possible transition)

Đây là test quan trọng NHẤT vì domain invariants bảo vệ data integrity.

**LoopStatus transition matrix** (10 states, 100 possible transitions):

```
Rust implementation should test ALL 100 transitions (25 valid, 75 invalid).
```

```rust
#[rstest]
#[case(LoopStatus::Idle, LoopStatus::Queued, Ok(()))]
#[case(LoopStatus::Idle, LoopStatus::Terminated, Ok(()))]
#[case(LoopStatus::Idle, LoopStatus::Running, Err(...))]
// ... 97 more cases
fn test_loop_status_transition_matrix(
    #[case] from: LoopStatus,
    #[case] to: LoopStatus,
    #[case] expected: Result<(), TransitionError>,
) {
    let result = assert_loop_status_transition(from, to);
    assert_eq!(result.map_err(|e| e.kind()), expected.map_err(|e| e.kind()));
}
```

**RunStatus transition matrix** (7 states, 49 possible transitions):

7 valid transitions tested exhaustively, 42 invalid transitions tested for rejection.

**ResumePolicy default selection**:
Test tất cả failure kinds kết hợp với resume policies:
```
retryable_transient + empty -> replay_step
retryable_after_resume + empty -> advance_from_checkpoint
manual_intervention + empty -> manual_intervention
non_retryable + empty -> replay_step
...các explicit overrides
```

### 4.2 Config loading + validation (all formats, all error cases)

**Must port from Go**:
- 300+ validation rules
- 3 format roundtrip (TOML -> struct -> YAML -> struct -> JSON)
- 30+ env var bindings
- Partial merge: defaults -> file -> env -> CLI
- Deprecated env/CLI warnings
- Protected instruction phrase blocking (20+ phrases)

**Must ADD for Rust** (bổ sung cho Go weaknesses):
- TOML format specific edge cases (integer overflow, enum variants)
- Rust type-specific validation (CIDR validation, URI parsing)
- Config file not found: graceful fallback vs error (differs from Go)

### 4.3 Queue claim atomicity (concurrent access patterns)

Go không có concurrent queue tests. Rust cần:

```rust
#[tokio::test]
async fn test_concurrent_queue_claim() {
    let storage = Arc::new(InMemoryStorage::new());
    let mut handles = vec![];
    
    // Spawn 10 concurrent claimers
    for _ in 0..10 {
        let storage = storage.clone();
        handles.push(tokio::spawn(async move {
            storage.claim_next_item().await
        }));
    }
    
    // Each item claimed exactly once
    let results: Vec<_> = futures::future::join_all(handles).await;
    let claimed: HashSet<_> = results.into_iter()
        .filter_map(|r| r.unwrap())
        .map(|item| item.id)
        .collect();
    
    assert_eq!(claimed.len(), 10); // No duplicate claims
}
```

**Những gì cần verify**:
- `ClaimNextNonLongTermRetry` và `ClaimNextLongTermRetry` không claim cùng item
- Lock acquirement trước khi claim
- Concurrent claim không vi phạm `OneRunningRunPerLoop`
- Queue priority ordering giữa concurrent claims

### 4.4 Recovery pipeline (startup with dangling runs)

**Test scenarios**:
1. **Clean startup**: No dangling runs, startup completes nhanh
2. **Orphan agent cleanup**: PID exists + command matches -> SIGTERM -> verify event
3. **Uncertain process identity**: PID exists + command doesn't match -> skip, event logged
4. **Stale run (startup mode)**: All running runs are candidates -> interrupt + requeue
5. **Stale run (live mode)**: Only runs with heartbeat > 30min are candidates
6. **Recent heartbeat**: Run with heartbeat < 5min -> skip
7. **Expired lock release**: Locks with expired TTL -> release, event logged
8. **Loop normalization**: running -> failed loop with manual_intervention queue item
9. **Deferred reviewer recovery**: Wait for login -> requeue

**Critical invariant**: Recovery không kill live processes, không corrupt data.

### 4.5 Runner step sequences (each runner, each step)

Mỗi runner có step pipeline riêng:

**Planner (5 steps)**: discover-issues -> prepare-worktree -> write-spec -> publish -> notify
**Reviewer (6 steps)**: discover -> filter -> claim -> snapshot -> review -> publish
**Fixer (10 steps)**: discover-pr -> claim-pr -> collect-fixes -> prepare-worktree -> repair -> validate -> push -> reconcile-commits -> resolve-comments -> recheck
**Worker (6 steps)**: prepare-work -> prepare-worktree -> plan -> execute -> validate -> open-pr

**Must test**:
1. Each step executes in correct order
2. Skip on checkpoint (advance_from_checkpoint)
3. Error in step N: classify failure, set resume policy
4. Resume: start from correct step
5. Step skipReason propagation
6. Checkpoint persistence after each step

### 4.6 Coordinator triage decision + dispatch

**Triage**:
- Issue with `looper:plan` label -> `ShouldTriage` = true
- Already triaged issue -> `ShouldTriage` = false, `ShouldReTriage` = true (if state changed)
- Issue with `looper:untriaged` + features -> `ShouldTriage` = true
- Issue outside project scope -> `isOutOfScope` = true
- Triage LLM decision: valid dispositions, label add/remove, comment

**Dispatch (human-gated)**:
- `/plan` slash command parsing (code fence/blockquote filter, permission check)
- Dispatch label validation (exactly one "dispatch/plan" or "dispatch/implement")
- Dependency gate: blocked issue -> failure comment
- Success path: trigger labels applied, assignee set, reaction added

**Dispatch (autonomous)**:
- Hold label veto: skip
- Autonomous delay: not yet eligible -> skip
- Dependency blockers: wait
- Success: trigger labels applied

### 4.7 Webhook event routing (every event type -> lane mapping)

Go không có integration test cho webhook routing. Rust cần:

```rust
#[tokio::test]
async fn test_webhook_push_event_routes_to_fixer() {
    let webhook = WebhookReceiver::new();
    let fixer = MockFixerRunner::new();
    let reviewer = MockReviewerRunner::new();
    
    webhook.register_handler("push", fixer.clone());
    webhook.register_handler("pull_request", reviewer.clone());
    
    // Send push event
    webhook.dispatch(PushEvent {
        repo: "acme/looper".into(),
        ref_name: "refs/heads/main".into(),
        commits: vec![Commit { id: "abc".into() }],
    }).await;
    
    assert!(fixer.was_called());
    assert!(!reviewer.was_called());
}
```

**Event-to-lane mapping**:
| Event | Lane | Action |
|-------|------|--------|
| `push` (base branch) | Fixer | Discover PRs needing base branch update |
| `pull_request.opened` | Reviewer | New PR to review |
| `pull_request.synchronize` | Reviewer/Fixer | Re-review or re-fix |
| `pull_request.closed` | Coordinator | Merge watch trigger |
| `pull_request.review_requested` | Reviewer | New review needed |
| `check_run.completed` | Fixer | Recheck triggered |
| `issue_comment.created` | Fixer | New fix item from comment |
| `pull_request_review.submitted` | Fixer | New review feedback |

### 4.8 Network join/heartbeat/leave

**Must test**:
1. Node join: Register with loopernet server, get assignment
2. Heartbeat: Periodic keepalive, server tracks liveness
3. Lease acquire: Distributed lock via loopernet
4. Lease renew: Refresh before expiry
5. Lease expire: Release on inactivity
6. Leave: Graceful deregistration
7. Identity drift: Node identity changes -> re-register
8. Event subscription: SSE stream receives cluster events

### 4.9 Worktree lifecycle (create, restore, cleanup)

**Create**:
- Branch name construction: `looper/planner/{issue}-{slug}`
- Base branch checkout
- Worktree directory under `worktreeRoot`
- Git worktree command parameters

**Restore** (resume path):
- Existing worktree detected
- Verify branch/head matches checkpoint
- Handle dirty worktree -> manual_intervention
- Handle stale worktree -> recreate

**Cleanup**:
- Plan: cross-reference loops/runs/queue for each worktree
- Retention window calculation
- Protected status detection
- Orphan detection
- Run: safety validation before git operations
- Dirty worktree protection

### 4.10 Agent execution (vendor flags, timeout, resume)

**5 vendors x 2 modes (fresh + native resume) = 10 test cases**:

| Vendor | Fresh args | Resume args |
|--------|-----------|-------------|
| claude-code | `--model X --print <prompt>` | `--resume <session> --print <prompt>` |
| codex | `exec --model X <prompt>` | `exec resume <session> <prompt>` |
| opencode | `run --model X --dir <cwd> <prompt>` | `run --session <session> --dir <cwd> <prompt>` |
| cursor-cli | `--model X --print <prompt>` | `--resume <session> --print <prompt>` |
| hermes | `-m X -z <prompt>` | (unsupported, falls back to fresh) |

**Must test**:
1. Model flag positioning (prepend vs after exec/run)
2. Prompt flag handling (--print, -z, inline, stdin)
3. Custom args + defaults (--profile, --dangerously-skip-permissions)
4. Environment variable construction (unsafe git keys stripped, LOOPER_PROMPT set)
5. Completion marker parsing (valid JSON, invalid, missing, template)
6. Native session ID extraction (JSON line, key=value)
7. Timeout handling: idle vs max_runtime vs none
8. finalStatus: exit code + timeout + kill combinations
9. Process group management: SIGTERM -> grace -> SIGKILL
10. Native resume fallback on failure

---

## 5. Test Infrastructure Design for Rust

### 5.1 MockGitHub Server

**Thiết kế**:

```rust
use axum::{Router, Json, response::IntoResponse};
use std::sync::{Arc, RwLock};

pub struct MockGitHubServer {
    pub addr: SocketAddr,
    shutdown: oneshot::Sender<()>,
}

impl MockGitHubServer {
    /// Start mock server on random port, return handle
    pub async fn start(state: MockGitHubState) -> Self;
    
    /// URL for octocrab client
    pub fn base_url(&self) -> String;
}

#[derive(Clone)]
pub struct MockGitHubState {
    inner: Arc<RwLock<MockGitHubStateInner>>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct MockGitHubStateInner {
    pub issues: HashMap<String, MockIssue>,
    pub pull_requests: HashMap<String, MockPullRequest>,
    pub review_threads: HashMap<String, MockReviewThread>,
    pub labels: HashMap<String, MockLabel>,
    pub current_user: String,
    pub rate_limit_remaining: u32,
}

impl MockGitHubState {
    pub fn new() -> Self;
    pub fn seed_issue(&self, key: &str, issue: MockIssue);
    pub fn seed_pull_request(&self, key: &str, pr: MockPullRequest);
    pub fn seed_review_thread(&self, key: &str, thread: MockReviewThread);
    
    /// Enable/disable rate limiting simulation
    pub fn set_rate_limit(&self, remaining: u32);
    
    /// Simulate transient error
    pub fn set_transient_error_rate(&self, probability: f64);
}

// Axum router builder
pub fn mock_github_router(state: MockGitHubState) -> Router {
    Router::new()
        .route("/repos/:owner/:repo/pulls", get(list_pull_requests))
        .route("/repos/:owner/:repo/pulls/:number", get(view_pull_request))
        .route("/repos/:owner/:repo/issues", get(list_issues))
        .route("/repos/:owner/:repo/issues/:number", get(view_issue))
        .route("/repos/:owner/:repo/issues/:number/comments", get(list_issue_comments))
        .route("/graphql", post(handle_graphql))
        .with_state(state)
}
```

**Endpoints mocks**:

| HTTP Endpoint | Method | Octocrab method |
|--------------|--------|----------------|
| `/repos/{o}/{r}/pulls` | GET | `pulls.list` |
| `/repos/{o}/{r}/pulls/{n}` | GET | `pulls.get` |
| `/repos/{o}/{r}/issues` | GET | `issues.list` |
| `/repos/{o}/{r}/issues/{n}` | GET | `issues.get` |
| `/repos/{o}/{r}/issues/{n}/comments` | GET | `issues.list_comments` |
| `/repos/{o}/{r}/labels` | GET | `issues.list_labels` |
| `/repos/{o}/{r}/pulls/{n}/reviews` | POST | `pulls.create_review` |
| `/graphql` | POST | `graphql` |

**Lợi thế so với Go version**:
- Octocrab gọi REST/GraphQL API trực tiếp, không qua gh CLI shell
- Không cần field allowlist validation (type safety từ octocrab response types)
- Response structs được define bằng serde, compile-time validation
- Có thể run in-process (không cần child process)

**Hạn chế**:
- Không test compatibility với gh CLI
- Octocrab client behavior khác với `exec("gh", ...)` (timeout, error format)
- Cần riêng contract test cho octocrab compatibility với real API

### 5.2 MockAgent

Thiết kế khác với Go version (không phải child process, implement trait):

```rust
#[async_trait]
pub trait AgentExecutor: Send + Sync {
    async fn execute(&self, input: RunInput) -> Result<AgentResult, AgentError>;
    async fn kill(&self, reason: &str) -> Result<()>;
}

// Mock implementations for each test scenario
pub struct MockAgentSuccess;
pub struct MockAgentTransientFailure;
pub struct MockAgentCommitWithReview;
pub struct MockAgentTimeout;

#[async_trait]
impl AgentExecutor for MockAgentSuccess {
    async fn execute(&self, input: RunInput) -> Result<AgentResult, AgentError> {
        Ok(AgentResult {
            status: "completed".to_string(),
            summary: "mock agent completed successfully".to_string(),
            changed_files: vec![],
            commits: vec![],
            parse_status: Some("parsed".to_string()),
            completion_signal: Some("__LOOPER_RESULT__=".to_string()),
            ..Default::default()
        })
    }
    
    async fn kill(&self, _reason: &str) -> Result<()> {
        Ok(())
    }
}

// Evidence collector
#[derive(Default)]
pub struct MockAgentEvidence {
    pub invocations: Vec<AgentInvocation>,
}

#[derive(Serialize)]
pub struct AgentInvocation {
    pub cwd: PathBuf,
    pub prompt: String,
    pub working_directory: PathBuf,
    pub timeout: Duration,
    pub native_resume_session_id: Option<String>,
    pub timestamp: DateTime<Utc>,
}

pub struct MockAgentWithEvidence {
    mode: AgentMode,
    evidence: Arc<Mutex<MockAgentEvidence>>,
    writable_files: Vec<PathBuf>,
}

impl MockAgentWithEvidence {
    pub fn new(mode: AgentMode) -> Self;
    
    /// Files the mock agent will create/modify
    pub fn with_writable_files(files: Vec<PathBuf>) -> Self;
    
    /// Capture evidence for assertions
    pub fn take_evidence(&self) -> MockAgentEvidence;
}
```

**Evidence pattern**: Thay vì ghi file như Go version, Rust mock agent lưu evidence trong `Arc<Mutex<...>>`. Test có thể read evidence sau khi test hoàn thành và verify:
- `cwd` is worktree directory
- `working_directory` matches expected
- `prompt` contains expected content
- Agent was NOT invoked for closed/blocked targets

### 5.3 TestConfig Builder

```rust
pub struct TestConfigBuilder {
    config: Config,
    overrides: HashMap<String, serde_json::Value>,
}

impl TestConfigBuilder {
    pub fn new() -> Self;
    
    /// Set port, default to random free port
    pub fn with_port(mut self, port: u16) -> Self;
    
    /// Set agent vendor and command
    pub fn with_agent(mut self, vendor: AgentVendor, command: &str) -> Self;
    
    /// Set agent env vars
    pub fn with_agent_env(mut self, env: HashMap<String, String>) -> Self;
    
    /// Add agent env var for fake tools
    pub fn with_fake_tool_env(mut self, key: &str, value: &str) -> Self;
    
    /// Set tool paths (git, gh, osascript)
    pub fn with_tool_paths(mut self, git: &str, gh: &str, looper: &str) -> Self;
    
    /// Add project to config
    pub fn with_project(mut self, project: ProjectRefConfig) -> Self;
    
    /// Set scheduler interval
    pub fn with_scheduler_interval(mut self, seconds: u64) -> Self;
    
    /// Set disclosure disabled
    pub fn with_disclosure_disabled(mut self) -> Self;
    
    /// Add raw JSON override (for legacy fields, etc.)
    pub fn with_raw_override(mut self, key: &str, value: serde_json::Value) -> Self;
    
    /// Write config to file, return path
    pub fn write_to(&self, dir: &Path) -> PathBuf;
    
    /// Build Config object (no file)
    pub fn build(&self) -> Config;
}

// Quick builder for common configs
pub fn fixer_config_with_fake_tools(
    home: &TempHome,
    agent_mode: &str,
    git_path: &str,
    gh_path: &str,
) -> Config;
```

### 5.4 InMemoryStorage

```rust
use rusqlite::Connection;
use tempfile::TempDir;

pub struct TestStorage {
    _tmpdir: TempDir,
    pub conn: Connection,
    pub db_path: PathBuf,
}

impl TestStorage {
    /// Create in-memory SQLite database with all migrations applied
    pub fn new() -> Self {
        let tmpdir = TempDir::new().unwrap();
        let db_path = tmpdir.path().join("looper_test.sqlite");
        let conn = Connection::open(&db_path).unwrap();
        run_migrations(&conn).unwrap();
        Self { _tmpdir: tmpdir, conn, db_path }
    }
    
    /// Create storage with partial migrations (for testing specific schema versions)
    pub fn with_migrations_up_to(version: i64) -> Self;
    
    /// Quick seed helpers
    pub fn seed_loop(&self, loop_record: LoopRecord);
    pub fn seed_run(&self, run_record: RunRecord);
    pub fn seed_queue_item(&self, item: QueueItemRecord);
    pub fn seed_worktree(&self, worktree: WorktreeRecord);
    
    /// Transaction helpers
    pub fn with_tx<F, R>(&self, f: F) -> R
    where F: FnOnce(&Transaction) -> R;
}
```

**Lưu ý cho Rust**:
- `rusqlite::Connection` không phải `Send` (SQLite C API constraint). Cần `Mutex<Connection>` hoặc connection pool.
- Dùng `tempfile::TempDir` để auto-clean database files.
- Migrations: dùng `refinery` crate với `rusqlite` feature.

### 5.5 Shared Test Utilities

```rust
// tests/common/mod.rs
pub mod mock_github;
pub mod mock_agent;
pub mod config_builder;
pub mod test_storage;
pub mod temp_home;
pub mod git_helpers;

/// Wait for a condition with timeout
pub async fn wait_until<F, Fut>(timeout: Duration, f: F) 
where F: Fn() -> Fut, Fut: Future<Output = bool>;

/// Poll API endpoint until expected response
pub async fn wait_for_api<T, F>(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
    predicate: F,
) -> T
where F: Fn(&serde_json::Value) -> Option<T>;

/// Assert invocation log contains expected call
pub fn assert_api_call(
    invocations: &[ApiInvocation],
    method: &str,
    path_pattern: &str,
);
```

---

## 6. CI/CD Testing Pipeline

### 6.1 What runs on every PR

```yaml
# .github/workflows/ci.yml
jobs:
  check:
    # cargo fmt, cargo clippy, cargo test (unit tests)
    
  test:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest]
    
    steps:
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      
      # Layer 1: Unit tests (fast, no external deps)
      - run: cargo test --workspace --lib --bins
      
      # Layer 2: Integration tests (in-memory SQLite, mock services)
      - run: cargo test --workspace --test '*'
      
      # Layer 4: Snapshot tests
      - run: cargo insta test --workspace --review
      
      # Layer 5: Property tests (--release for performance)
      - run: cargo test --release --workspace -- proptest_
      
      # Fuzz tests (short run per PR)
      - run: cargo fuzz run --fuzz-dir fuzz config_parser -- -max_len=1024 -runs=10000
      
      # Build check (no E2E binary build yet)
      - run: cargo build --all-targets
```

**PR test timing estimate**:
| Stage | Time | Parallelism |
|-------|------|------------|
| cargo fmt + clippy | 2-3 min | 1 job |
| Unit tests | 3-5 min | 4 test-threads |
| Integration tests | 5-8 min | 2 test-threads |
| Snapshot tests | 1-2 min | 4 test-threads |
| Property tests | 3-5 min | 2 test-threads (release mode) |
| Fuzz (short) | 2-3 min | 1 job |
| Build check | 3-5 min | 1 job |
| **Total** | **~15-25 min** | |

### 6.2 What runs on merge to main

```yaml
# Additional jobs triggered by push to main
jobs:
  full-test:
    # All layers including E2E
    - run: cargo test --workspace --all-targets
    - run: cargo test --release --workspace -- proptest_ -- --ignored
    - run: cargo insta test --workspace --review --accept  # Update snapshots
    
  e2e:
    # Layer 3: Binary-level E2E (pre-built binaries)
    - run: |
        cargo build --package looperd --release
        cargo build --package looper-cli --release
    - run: LOOPER_E2E_LOOPERD_PATH=./target/release/looperd cargo test --test e2e -- --test-threads=1
    
  fuzz:
    # Full fuzz run
    - run: cargo fuzz run --fuzz-dir fuzz config_parser -- -max_len=4096 -runs=100000
    - run: cargo fuzz run --fuzz-dir fuzz diffanchor_parser -- -max_len=4096 -runs=100000
    - run: cargo fuzz run --fuzz-dir fuzz json_response_parser -- -max_len=8192 -runs=50000
```

### 6.3 What runs on release tag

```yaml
# Trigger: v*.*.*
jobs:
  full-matrix:
    strategy:
      matrix:
        target: [x86_64-unknown-linux-musl, aarch64-apple-darwin, x86_64-apple-darwin]
    
    steps:
      - run: cargo test --all-targets --target ${{ matrix.target }}
      - run: LOOPER_E2E_REAL_GH=1 cargo test --test e2e -- --test-threads=1
      - run: cargo insta test --workspace --review --accept
      - run: cargo fuzz run --fuzz-dir fuzz config_parser -- -max_len=4096 -runs=200000
```

### 6.4 Path-filter based E2E targeting

Như Go E2E design, Rust cũng nên có path-filter để chỉ chạy E2E khi relevant files thay đổi:

```yaml
jobs:
  path-filter:
    outputs:
      storage: ${{ steps.filter.outputs.storage }}
      github: ${{ steps.filter.outputs.github }}
      runner: ${{ steps.filter.outputs.runner }}
      runtime: ${{ steps.filter.outputs.runtime }}
      daemon: ${{ steps.filter.outputs.daemon }}
    
    steps:
      - uses: dorny/paths-filter@v3
        id: filter
        with:
          filters: |
            storage:
              - 'crates/looper-storage/**'
            github:
              - 'crates/looper-github/**'
            runner:
              - 'crates/looper-runner/**'
            runtime:
              - 'crates/looper-scheduler/**'
            daemon:
              - 'crates/looperd/**'

  e2e-storage:
    if: ${{ needs.path-filter.outputs.storage == 'true' }}
    runs-on: ubuntu-latest
    steps:
      - run: cargo test --test e2e -- storage_
```

### 6.5 Parallel test execution strategy

```toml
# .cargo/config.toml
[test]
# Default: use logical processors, but for E2E use --test-threads=1
```

```bash
# Unit + integration tests: parallel by crate
cargo test --workspace --lib --bins --test-threads=8  # 8 parallel test threads

# Integration tests: separate binary run
cargo test --workspace --test '*integration*' --test-threads=4

# E2E tests: sequential
cargo test --test e2e -- --test-threads=1

# Property tests: sequential per test
cargo test --release -- proptest_ --test-threads=1
```

**Test isolation**:
- Mỗi integration test dùng `TempDir` hoặc in-memory SQLite riêng
- E2E tests dùng port = 0 (OS assigns) để tránh conflict
- Mỗi E2E test có temp HOME riêng

---

## 7. Estimated Effort

### 7.1 Total test code estimate

| Layer | Estimated lines | Notes |
|-------|---------------|-------|
| Layer 1: Unit tests | 8,000-12,000 | Per-crate, per-module |
| Layer 2: Integration tests | 6,000-10,000 | Cross-crate, mock IO |
| Layer 3: E2E tests | 3,000-5,000 | Binary-level |
| Layer 4: Snapshot tests | 1,000-2,000 | insta golden files |
| Layer 5: Property/Fuzz | 1,000-2,000 | proptest, cargo-fuzz |
| Test infrastructure | 2,000-3,000 | Mock servers, harness |
| **Total** | **21,000-34,000** | |

### 7.2 Implementation effort

| Phase | Effort | Dependencies | Parallelizable |
|-------|--------|-------------|---------------|
| Test infrastructure build | 2 weeks | None (foundation for all layers) | Limited (sequential design) |
| Layer 1: Unit tests | 1.5-2 weeks | Infrastructure complete | YES (per crate, parallel) |
| Layer 2: Integration tests | 2-2.5 weeks | Layer 1 + services implemented | Partial (by service) |
| Layer 3: E2E tests | 2-3 weeks | All crates implemented | Limited (depends on full system) |
| Layer 4: Snapshot tests | 0.5 week | CLI + API implemented | YES |
| Layer 5: Property/Fuzz | 0.5-1 week | Core types stable | YES |
| **Total** | **8.5-11 weeks** | | |

**Note**: These estimates assume tests are written **in parallel with or after** the Rust implementation. If tests are written incrementally (each crate's tests done during its implementation), the total calendar time is shorter.

### 7.3 Risk-adjusted estimate

| Scenario | Best case | Expected | Worst case |
|----------|-----------|----------|------------|
| Test infrastructure | 1.5 weeks | 2 weeks | 3 weeks |
| Unit tests | 1 week | 1.5 weeks | 2.5 weeks |
| Integration tests | 1.5 weeks | 2 weeks | 3 weeks |
| E2E tests | 1.5 weeks | 2.5 weeks | 4 weeks |
| Snapshot + Property | 1 week | 1 week | 1.5 weeks |
| **Total** | **6.5 weeks** | **9 weeks** | **14 weeks** |

**Key risk factors**:
- Rust async testing new patterns (learning curve for complex mock setups)
- Platform-specific issues (macOS vs Linux process management)
- SQLite concurrency model differences (Go's `database/sql` vs Rust's `rusqlite` + `r2d2`)
- Test binary compilation time (16 crates -> ~5-8 min per clean build)
- Flaky E2E tests (timeout tuning, race conditions)

---

## 8. Risks and Mitigations

### 8.1 Rust ownership model makes shared test state harder than Go

**Risk**: Go dùng `testing.TB` + global state + shared mocks dễ dàng. Rust ownership model làm shared mutable state khó hơn.

**Mitigations**:
- Dùng `Arc<RwLock<T>>` cho shared state (mock GitHub state, evidence collector)
- Dùng `tokio::sync::Mutex` cho async shared state
- Mỗi test function nhận own state (không global mutable)
- Test infrastructure traits design: các mock implement trait, được `Arc` wrapped
- Tránh `unsafe` hoàn toàn trong test code

### 8.2 async Rust testing is more complex than goroutine testing

**Risk**: Go goroutines dễ dùng. Rust async testing cần tokio runtime, `#[tokio::test]`, careful `Send` bounds.

**Mitigations**:
- `#[tokio::test]` cho tất cả async tests
- `#[tokio::test(flavor = "multi_thread")]` cho concurrent tests
- `tokio::time::pause()` cho timeout tests (không cần actual wait)
- `tokio_test::block_on` cho integration tests
- Dùng `futures::future::join_all` cho concurrent assertions
- Tránh `tokio::spawn` trong test unless necessary (prefer sequential in tests)

### 8.3 SQLite in Rust: connection pool vs single connection trade-offs

**Risk**: Go's `database/sql` được design cho connection pools. Rust's `rusqlite` có single `Connection` không `Send`.

**Mitigations**:
- Unit tests: single connection with `Mutex` (fast, simple)
- Integration tests: `r2d2` connection pool or `deadpool-sqlite`
- Migration tests: single connection with serialized access
- E2E tests: daemon manages own pool, test connects via API
- Transaction tests: `rusqlite::Transaction` directly (not through pool)

### 8.4 Cross-platform test differences

**Risk**: macOS vs Linux differences in:
- Process management (signal behavior, `/proc` unavailability)
- Filesystem case sensitivity
- Temp directory behavior
- git version differences

**Mitigations**:
- CI matrix: ubuntu-latest + macos-latest (Windows: stretch goal)
- Process tests dùng platform-specific helpers
- Signal handling tests: conditional compilation (`#[cfg(target_os = "linux")]`)
- `/proc` không available on macOS -> dùng `ps -p {pid} -o command=`
- Temp directory: dùng `tempfile::TempDir` cross-platform
- git version detection: `git --version` check in helpers

### 8.5 Test binary compilation time

**Risk**: 16 crates -> full `cargo test` compile time 5-8 mins.

**Mitigations**:
- `cargo test --workspace --lib --bins` (skip integration test compilation)
- `cargo test -p looper-core` (per-crate for faster iteration)
- `cargo watch -x test` during development
- CI dùng `Swatinem/rust-cache@v2` để cache dependencies
- E2E binary pre-built, không compile trong test run
- `sccache` for developer machines

### 8.6 Flaky tests trong async integration

**Risk**: Async tests dễ flaky do timing issues, race conditions, timeout tuning.

**Mitigations**:
- `tokio::time::pause()` + `advance()` cho deterministic time tests
- Dùng `wait_until()` pattern với bounded retry thay vì `sleep()`
- Port random -> deterministic trong tests (seed random generators)
- `#[cfg(test)]` feature flags cho deterministic behavior
- CI retry test on failure (max 2)
- Flaky test tracking: mark với `#[ignore]` và tạo tracking issue

### 8.7 Mock complexity for 16+ service traits

**Risk**: Số lượng mock traits có thể rất lớn, dẫn đến maintenance overhead.

**Mitigations**:
- Dùng `mockall` crate cho auto-generated mocks (derive(#[automock]))
- Interface design: mỗi crate expose trait cho external dependencies
- Facade trait cho composite operations
- Dùng `default_impl` trên traits để reduce boilerplate
- Mock chỉ implement methods cần thiết cho test (không implement tất cả)

### 8.8 Go E2E harness vs Rust E2E approach

**Risk**: Go E2E dùng fake gh CLI binary (exec). Rust dùng octocrab (REST API). Không test được gh CLI compatibility.

**Mitigations**:
- Separate contract test layer: run real gh CLI against test endpoints
- octocrab client testing: use `wiremock` crate
- Periodic compatibility check: manually run against real GitHub API
- Acceptance criteria: "looper --help" and basic commands work with real gh

---

## 9. Prioritization và Implementation Roadmap

### Phase 1: Foundation (Week 1-2)

```
Priority: Test infrastructure > Unit tests > Integration > E2E > Snapshot > Property
```

1. Test infrastructure: `MockGitHubServer`, `MockAgent`, `TestStorage`, `TempHome`, `TestConfigBuilder`
2. Domain state machine unit tests (Layer 1, looper-core) — QUAN TRỌNG NHẤT
3. Config validation unit tests (Layer 1, looper-core-config)
4. GitHub gateway parsing tests (Layer 1, looper-github)

### Phase 2: Core Logic Tests (Week 3-5)

```
Bắt đầu khi các crate cốt lõi đã ổn định.
```

5. Storage repository tests (Layer 2, looper-storage)
6. Agent executor tests (Layer 1, looper-agent) — 5 vendors x 2 modes
7. Service layer integration tests (Layer 2, LoopService, RunService, ProjectService)
8. Coordinator submodule tests (Layer 2, triage, dispatch, depgraph, mergewatch)
9. Runner unit + integration tests (Layer 2, planner, reviewer, fixer, worker)

### Phase 3: System Tests (Week 6-8)

```
Khi scheduler và runtime đã implement.
```

10. Scheduler tick cycle integration tests (Layer 2)
11. Runtime recovery integration tests (Layer 2, stale run, orphan cleanup, etc.)
12. API layer integration tests (Layer 2, axum test utilities)
13. Network layer integration tests (Layer 2, wiremock)

### Phase 4: E2E và Finalization (Week 9-11)

```
Khi toàn bộ hệ thống có thể chạy như binary.
```

14. Daemon boot smoke E2E (Layer 3)
15. Planner/Reviewer/Fixer/Worker full cycle E2E (Layer 3)
16. Resolve-comments scenarios (Layer 3)
17. Worktree isolation E2E (Layer 3)
18. Snapshot tests (Layer 4, insta)
19. Property-based tests (Layer 5, proptest)
20. Fuzz targets (Layer 5, cargo-fuzz)

---

## Appendix A: Test Code Organization

```
crates/
  looper-core/
    src/
      domain/
        status.rs           // + #[cfg(test)] mod tests
        resume.rs           // + #[cfg(test)] mod tests
        target.rs           // + #[cfg(test)] mod tests
      config/
        validation.rs       // + #[cfg(test)] mod tests
        merge.rs            // + #[cfg(test)] mod tests
      disclosure/
        stamp.rs            // + #[cfg(test)] mod tests
      network_policy/
        policy.rs           // + #[cfg(test)] mod tests
  looper-storage/
    src/
      repositories.rs       // + #[cfg(test)] mod tests
      migrations.rs         // + #[cfg(test)] mod tests
    tests/
      queue_test.rs
      loops_test.rs
      locks_test.rs
  looper-github/
    src/
      gateway.rs            // + #[cfg(test)] mod tests
      discovery.rs          // + #[cfg(test)] mod tests
      errors.rs             // + #[cfg(test)] mod tests
  looper-git/
    src/
      gateway.rs            // + #[cfg(test)] mod tests
  looper-agent/
    src/
      executor.rs           // + #[cfg(test)] mod tests (large module)
      prompt.rs             // + #[cfg(test)] mod tests
    tests/
      timeout_test.rs       // Process group management
tests/
  common/
    mod.rs
    mock_github.rs
    mock_agent.rs
    config_builder.rs
    test_storage.rs
    temp_home.rs
    git_helpers.rs
  integration/
    service_loops_test.rs
    service_runs_test.rs
    service_projects_test.rs
    scheduler_test.rs
    runner_planner_test.rs
    runner_reviewer_test.rs
    runner_fixer_test.rs
    runner_worker_test.rs
    coordinator_test.rs
    runtime_test.rs
    recovery_test.rs
    webhook_test.rs
    network_test.rs
    api_test.rs
  e2e/
    daemon_boot_test.rs
    planner_full_test.rs
    reviewer_full_test.rs
    fixer_full_test.rs
    worker_full_test.rs
    worktree_isolation_test.rs
    resolve_comments_test.rs
    coordinator_test.rs
  snapshots/
    config_tests.rs
    cli_tests.rs
    api_tests.rs
  property/
    state_machine_tests.rs
    queue_priority_tests.rs
    config_merge_tests.rs
    depgraph_tests.rs
fuzz/
    Cargo.toml
    targets/
        config_parser.rs
        diffanchor_parser.rs
        json_response_parser.rs
```

## Appendix B: Key Cargo.toml dependencies cho testing

```toml
[dev-dependencies]
rstest = "0.23"
insta = { version = "1", features = ["yaml"] }
mockall = "0.13"
tempfile = "3"
proptest = "1"
wiremock = "0.6"
assert_cmd = "2"
predicates = "3"
reqwest = { version = "0.12", features = ["json"] }
tokio = { version = "1", features = ["test-util", "macros", "rt-multi-thread"] }
serde_json = "1"
serde_yaml = "0.9"
toml = "0.8"
cargo-insta = "1"  # CLI tool
cargo-fuzz = "0.12"  # CLI tool
```

## Appendix C: Go Coverage Gap Mapping

| Package | Go coverage | Rust priority | Rationale |
|---------|------------|---------------|-----------|
| `domain` | HIGH (exhaustive) | MUST | Foundation for all invariants |
| `config` | HIGH (300+ rules) | MUST | Prevent misconfiguration |
| `storage` | HIGH (migrations + repos) | MUST | Data integrity |
| `agent` | MEDIUM | MUST | 5 vendors, env safety, resume |
| `infra/github` | HIGH | MUST | gh CLI contract, parsing |
| `infra/git` | MEDIUM | MUST | Worktree, branch safety |
| `loops` | MEDIUM | MUST | Service layer invariants |
| `runs` | LOW | MUST | Run lifecycle integrity |
| `projects` | MEDIUM | MUST | Project CRUD, sync |
| `reviewer` | LOW | HIGH | Complex step pipeline |
| `fixer` | LOW | HIGH | 10-step, most complex |
| `worker` | LOW | HIGH | Worktree isolation |
| `planner` | LOW | HIGH | Spec generation |
| `coordinator` | LOW | HIGH | Triage + dispatch + depgraph |
| `runtime` | LOW | HIGH | Recovery, startup, lifecycle |
| `scheduler` | LOW | HIGH | Tick cycle, priority |
| `api` | HIGH (6.6k lines) | HIGH | Endpoint contract |
| `network` | LOW | MEDIUM | Join/heartbeat/lease |
| `cli` | MEDIUM | MEDIUM | User-facing commands |
| `e2e` | MEDIUM | MEDIUM | System integration |
| Fuzz/property | NONE | HIGH | Critical gaps in Go |

---

> **Final note**: Testing strategy này không phải là "viết xong test rồi mới implement". Mục tiêu là test được viết song song hoặc trước implementation (TDD cho critical invariants). Đầu tư vào test infrastructure ngay từ đầu sẽ trả nợ nhanh chóng khi phát hiện regression early. Rust's type system và testing ecosystem (insta, proptest, mockall) cho phép coverage vượt trội so với Go, nhưng đòi hỏi effort ban đầu lớn hơn.
