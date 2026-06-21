# Module: Service Layer — Rust Spec

> Derived from `internal/loops/service.go` (349 lines), `internal/runs/service.go` (317 lines),
> `internal/projects/service.go` (898 lines), `internal/loops/policy.go` (34 lines),
> `internal/domain/domain.go` (257 lines), `internal/storage/repositories.go` (~2000 lines),
> `internal/projects/reviewer_automerge_validation.go` (83 lines), `internal/eventlog/eventlog.go` (107 lines)

---

## 1. LoopService

### Struct

```rust
struct LoopService {
    db:    sqlite::Connection,   // or Pool
    repos: Arc<Repositories>,    // storage layer
    now:   fn() -> DateTime<Utc>,
}
```

### 1.1 Create

**Input:**
```rust
struct CreateInput {
    project_id:    String,
    r#type:        LoopType,        // planner | reviewer | worker | fixer
    target:        LoopTarget,      // project | issue | pull_request
    status:        LoopStatus,      // initial status
    config_json:   Option<String>,
    metadata_json: Option<String>,
}
```

**Behavior:**
1. Validate rằng `LoopType` matches `TargetType`:
   - `Planner` must target an `issue`
   - `Reviewer`, `Fixer` must target a `pull_request`
   - `Worker` may target `project`, `issue`, or `pull_request`
2. Fetch project by `project_id` — reject nếu project không tồn tại
3. Build `LoopSummary` list từ tất cả loops hiện có
4. Check unique active loop constraint:
   - Nếu candidate status KHÔNG phải active status (`idle`, `queued`, `running`, `paused`) → skip check
   - Nếu một loop active khác tồn tại với cùng `(project_id, type, target_key)` → reject với conflict error
   - **Ngoại lệ**: Concurrent project-scoped workers (`LoopTypeWorker` + `TargetTypeProject`) được phép chạy đồng thời
5. Allocate `seq` — atomic increment qua SQLite counter table (`counters.name = 'loop_seq'`)
6. Build `LoopRecord`:
   - `id` = `NewEventID("loop")` (16-byte random hex)
   - `seq` = allocated
   - `status` = từ input
   - `created_at` = `updated_at` = now
   - Nếu `status == Running` → set `next_run_at = now`
7. Upsert loop record trong transaction
8. Return `LoopRecord`

**Errors:**
- Project not found
- Loop type / target type mismatch
- Conflicting active loop already exists
- Seq allocation failure

### 1.2 Get / GetBySeq / List

**Behavior:**
- `Get(id)`: delegate xuống `repos.Loops.GetByID(ctx, id)`
- `GetBySeq(seq)`: delegate xuống `repos.Loops.GetBySeq(ctx, seq)`
- `List()`: delegate xuống `repos.Loops.List(ctx)` — trả về tất cả loops, ordered by `updated_at DESC, seq DESC`

**Errors:**
- Repository chưa được configure → "loops repository is not configured"

### 1.3 TransitionStatus

**Input:**
```rust
struct TransitionInput {
    status:      LoopStatus,
    next_run_at: Option<DateTime<Utc>>,
    last_run_at: Option<DateTime<Utc>>,
}
```

**Behavior:**
1. Fetch loop by ID — reject nếu không tìm thấy
2. Validate transition qua `AssertLoopStatusTransition(from, to)`
3. **Allowed transitions** (full matrix):

```
idle        → queued, paused, terminated
queued      → running, paused, terminated
running     → completed, failed, paused, interrupted, waiting, terminated
paused      → queued, completed, stopped, terminated
waiting     → queued, paused, stopped, terminated
stopped     → (terminal — no outgoing transitions)
terminated  → (terminal)
completed   → (terminal)
failed      → (terminal)
interrupted → queued, failed
```

4. Cập nhật record:
   - `status` = input.status
   - `updated_at` = now
   - `next_run_at`:
     - Nếu `next_run_at` được set explicit → dùng giá trị đó
     - Nếu chuyển sang `queued` mà không set → `next_run_at = now` (schedule immediately)
     - Nếu chuyển sang `running` → giữ nguyên hoặc từ input
     - Nếu chuyển sang trạng thái khác (không `running`, không `queued`) → `next_run_at = None`
   - `last_run_at`: set nếu input có giá trị
5. Upsert trong transaction, return record

**Errors:**
- Loop not found
- Invalid transition (ví dụ: `queued → completed`)

### 1.4 Pause

**Input:**
```rust
struct PauseInput {
    loop_id: String,
    reason:  Option<String>,
}

struct PauseResult {
    loop:                 LoopRecord,
    cancelled_queue_items: i64,
}
```

**Behavior:**
1. Fetch loop — reject nếu không tìm thấy
2. If loop status is NOT already `paused` → validate transition từ status hiện tại sang `paused`
3. Update loop:
   - `status = paused`
   - `next_run_at = None` (clear schedule)
   - `updated_at = now`
4. **Side effect**: Cancel tất cả active queue items thuộc loop này:
   - `repos.Queue.CancelByLoop(loop_id, now, reason)`
   - Set `status = cancelled`, `finished_at = now`, `last_error = reason`
   - Target: items with status IN (`queued`, `running`)
5. Return `PauseResult` với loop record + số lượng queue items bị cancel

**Errors:**
- Loop not found
- Invalid transition (nếu status hiện tại không thể pause)

### 1.5 Terminate

**Input:**
```rust
struct TerminateInput {
    loop_id: String,
    reason:  Option<String>,
}

struct TerminateResult {
    loop:                 LoopRecord,
    cancelled_queue_items: i64,
}
```

**Behavior:**
- Identical pattern với Pause, nhưng status target là `terminated`
- Cũng cancel toàn bộ active queue items

**Errors:**
- Loop not found (including already-terminated)
- Invalid transition

### 1.6 Resume

**Input:**
```rust
fn Resume(loop_id: String) -> Result<LoopRecord>
```

**Behavior:**
1. Directly delegate tới `TransitionStatus` với `status = queued`, `next_run_at = now`
2. Điều này đặt loop quay lại hàng đợi với lịch schedule ngay lập tức

**Errors:**
- Loop not found
- Transition `paused → queued` không hợp lệ (nếu loop không ở trạng thái paused)

### 1.7 ResumePolicy — Đầy đủ 6 giá trị

```rust
const RESUME_POLICY_ADVANCE_FROM_CHECKPOINT: &str = "advance_from_checkpoint";
const RESUME_POLICY_MANUAL_INTERVENTION:     &str = "manual_intervention";
const RESUME_POLICY_REPLAY_STEP:             &str = "replay_step";
const RESUME_POLICY_RESTART_FROM_DISCOVER:  &str = "restart_from_discover";
const RESUME_POLICY_RERUN_REVIEW:           &str = "rerun_review";
const RESUME_POLICY_RETRY_FROM_TIMEOUT_CONTEXT: &str = "retry_from_timeout_context";
```

**Ngữ nghĩa từng policy:**

| Policy | Behavior khi Loop được resume |
|--------|-------------------------------|
| `advance_from_checkpoint` | Step execution bắt đầu từ step tiếp theo sau last completed step. Checkpoint được giữ nguyên. Đây là default cho `FailureKindRetryableAfterResume` khi không có resume policy explicit |
| `manual_intervention` | Loop không tự động resume. Chỉ resume khi người dùng tương tác qua API/CLI. Không enqueue tự động |
| `replay_step` | Step execution bắt đầu lại từ step đầu tiên của runner. Checkpoint bị reset. Đây là default cho `FailureKindRetryableTransient` và các failure kind khác không được xử lý đặc biệt |
| `restart_from_discover` | Runner khởi động lại từ bước discover (ví dụ: re-fetch PR detail, re-evaluate filter conditions). Dùng khi PR head thay đổi hoặc context cần refresh |
| `rerun_review` | Reviewer-specific: review lại toàn bộ PR mà không restart từ discover. Dùng khi review marker bị miss hoặc pending review bị clear |
| `retry_from_timeout_context` | Khởi động lại agent execution với native resume từ session context của timeout trước đó |

**NormalizeResumePolicy:**
```rust
fn normalize_resume_policy(failure_kind: &str, resume_policy: Option<&str>) -> &str {
    match (failure_kind, resume_policy) {
        (_, Some(policy)) if !policy.trim().is_empty() => policy,
        ("retryable_after_resume", _) => "advance_from_checkpoint",
        ("manual_intervention", _)   => "manual_intervention",
        _                            => "replay_step",
    }
}
```

### 1.8 SuppressesAutonomousRecovery

```rust
fn suppresses_autonomous_recovery(failure_kind: &str, resume_policy: &str) -> bool {
    // IsHardHold check: returns true nếu:
    //   1. resume_policy == "manual_intervention", HOẶC
    //   2. failure_kind == "manual_intervention"
    is_hard_hold(failure_kind, resume_policy)
}
```

**Behavior:**
- Được gọi bởi runtime trước khi tự động re-enqueue một failed run
- Nếu `true`: runtime sẽ KHÔNG tự động tạo queue item mới. Loop chỉ được resume qua API/CLI
- Nếu `false`: runtime có thể tự động re-enqueue theo policy

**Chi tiết:**
- `(failure_kind=manual_intervention, resume_policy=*)` → true
- `(*, resume_policy=manual_intervention)` → true
- `(retryable_after_resume, restart_from_discover)` → false (cho phép tự động restart từ discover)

### 1.9 ShouldRestartFromDiscover

```rust
fn should_restart_from_discover(status: &str, resume_policy: &str) -> bool {
    if status != "failed" && status != "interrupted" { return false; }
    resume_policy.trim() == "restart_from_discover"
}
```

**Behavior:**
- Được dùng ở các runner decision points để quyết định có nên chạy lại từ đầu hay không
- Chỉ apply khi run đã kết thúc với `failed` hoặc `interrupted`
- Nếu true: runner sẽ bỏ qua checkpoint, chạy step discover lại từ đầu thay vì advance từ checkpoint

---

## 2. RunService

### Struct

```rust
struct RunService {
    db:    sqlite::Connection,
    repos: Arc<Repositories>,
    loops: Arc<LoopService>,
    now:   fn() -> DateTime<Utc>,
}
```

### 2.1 StartRun

**Input:**
```rust
struct StartInput {
    loop_id:             String,
    current_step:        Option<String>,
    last_completed_step: Option<String>,
    checkpoint_json:     Option<String>,
}
```

**Behavior (atomic transaction):**
1. Validate service dependencies (db, repos, loops)
2. Fetch loop — reject nếu không tìm thấy
3. **OneRunningRunPerLoop enforcement**:
   - `repos.Runs.HasRunningByLoopID(loop_id)` → nếu `true` → reject với error "loop {id} already has a running run"
4. Validate loop status:
   - Nếu loop status không phải `running` → validate transition từ status hiện tại sang `running`
   - Nếu loop status đã là `running` → skip (cho phép)
5. Validate steps (nếu được cung cấp):
   - `current_step` và `last_completed_step` must belong to LoopType (via `AssertStepBelongsToLoopType`)
6. Tạo `RunRecord`:
   - `id` = `NewEventID("run")`
   - `loop_id` = từ input
   - `status = "running"`
   - `current_step`, `last_completed_step`, `checkpoint_json` = từ input
   - `started_at = now`
   - `last_heartbeat_at = now`
7. Upsert run record
8. **Side effect — Update loop**:
   - `loop.status = "running"`
   - `loop.last_run_at = now`
   - `loop.next_run_at = None`
9. **Side effect — Event log**:
   - Append `loop.started` event (`payload: {status: "running"}`)
   - Append `run.started` event (`payload: {currentStep, lastCompletedStep}`)
10. Return RunRecord

**Errors:**
- Loop not found
- Loop already has a running run
- Invalid loop status transition
- Current step / last completed step không thuộc loop type

### 2.2 RecordStep (Heartbeat)

**Input:**
```rust
struct RecordStepInput {
    run_id:             String,
    loop_type:          LoopType,
    current_step:       Option<String>,
    last_completed_step: Option<String>,
    checkpoint_json:    Option<String>,
    last_heartbeat_at:  Option<DateTime<Utc>>,
    event_type:         Option<String>,     // e.g. "loop.step.completed"
    event_payload:      Option<serde_json::Value>,
}
```

**Behavior:**
1. Fetch run — reject nếu không tìm thấy
2. Validate steps (nếu cung cấp): `current_step` và `last_completed_step` must belong to `loop_type`
3. Update run record:
   - `current_step`, `last_completed_step`, `checkpoint_json` = từ input (nếu Some)
   - `last_heartbeat_at` = `last_heartbeat_at` từ input, hoặc fallback xuống `now`
   - `updated_at` = thời gian heartbeat
4. Lấy loop từ `run.loop_id` để lấy `project_id`
5. **Side effect — Event log** (nếu `event_type` được cung cấp):
   - Append event với `event_type`, `project_id`, `loop_id`, `run_id`, `payload`
6. Return updated RunRecord

**Errors:**
- Run not found
- Step không thuộc loop type

### 2.3 Complete

**Input:**
```rust
struct CompleteInput {
    status:          RunStatus,    // success | failed | cancelled | interrupted | parse_failed
    summary:         Option<String>,
    error_message:   Option<String>,
    checkpoint_json: Option<String>,
}
```

**Behavior:**
1. Fetch run — reject nếu không tìm thấy
2. **Validate run status transition**:
```
queued       → running     (only allowed from queued)
running      → success, failed, cancelled, interrupted, parse_failed
success      → (terminal)
failed       → (terminal)
cancelled    → (terminal)
interrupted  → (terminal)
parse_failed → (terminal)
```
3. Update run record:
   - `status` = input status
   - `summary`, `error_message`, `checkpoint_json` = từ input
   - `ended_at = now`
   - `last_heartbeat_at = now`
4. **Side effect — Event log**:
   - Nếu `status == RunStatusSuccess` → event type = `run.completed`
   - Nếu `status != RunStatusSuccess` → event type = `run.failed`
   - Payload: `summary`, `errorMessage`
5. Return updated RunRecord

**Errors:**
- Run not found
- Invalid run status transition (ví dụ: `success → failed`)

### 2.4 Query methods

```rust
fn Get(id: &str) -> Result<Option<RunRecord>>;            // repos.Runs.GetByID
fn List() -> Result<Vec<RunRecord>>;                       // repos.Runs.List
fn ListByLoop(loop_id: &str) -> Result<Vec<RunRecord>>;   // repos.Runs.ListByLoop
fn LatestForLoop(loop_id: &str) -> Result<Option<RunRecord>>; // repos.Runs.GetLatestByLoopID
```

### 2.5 OneRunningRunPerLoop — Đảm bảo

**Implementation detail:**
- `repos.Runs.HasRunningByLoopID(loop_id)` thực thi: `SELECT COUNT(*) FROM runs WHERE loop_id = ? AND status = 'running'`
- Được gọi trong `StartRun` trước khi tạo run mới
- Nếu `count > 0` → reject với error
- **Lưu ý**: Kiểm tra nằm trong transaction, race condition được xử lý nhờ SQLite exclusive transaction

---

## 3. ProjectService

### Struct

```rust
struct ProjectService {
    db:                         sqlite::Connection,
    repos:                      Arc<Repositories>,
    logger:                     Option<Logger>,
    config:                     Config,
    now:                        fn() -> DateTime<Utc>,
    detect_repo:                Option<DetectRepoFn>,
    get_repository_settings:    Option<GetRepositorySettingsFn>,
    get_branch_protection:      Option<GetBranchProtectionFn>,
    list_worktrees:             Option<ListWorktreesFn>,
    list_open_pull_requests:    Option<ListOpenPullRequestsFn>,
    capture_pull_request_snapshot: Option<CapturePullRequestSnapshotFn>,
    async_snapshot_queue_enabled: fn() -> bool,
}
```

### 3.1 AddProject

**Input:**
```rust
struct AddInput {
    id:             String,
    name:           String,
    repo_path:      String,
    base_branch:    String,
    id_source:      String,        // "explicit" | "derived"
    worktree_root:  Option<String>,
    repo:           Option<String>, // explicit repo override
    snapshot_mode:  SnapshotMode,  // "async" (default) | "full" | "off"
}

struct AddResult {
    project:                ProjectRecord,
    repo:                   Option<String>,
    discovered_pull_requests: usize,
    discovered_worktrees:    usize,
    pending_snapshots:       usize,
    captured_snapshots:      usize,
    warnings:                Vec<String>,
}

enum SnapshotMode { Async, Full, Off }
```

**Behavior:**

#### Phase 1: Validation & ID Normalization
1. Check project tồn tại theo ID
2. **Collision detection**:
   - Nếu existing `!= None` và không archived và `id_source != "derived"` → `ProjectIDCollisionError`
   - Nếu existing `== None`:
     - Normalize project ID (nếu `id_source == "derived"`, replace all non-`[a-z0-9]` chars with `-`)
     - Nếu normalized ID khác input ID → check normalized ID có collision với explicit project không
     - Normalize legacy prefix: `legacy-id-*` → `project_legacy-id-*`
3. Validate project ID không được:
   - Empty, `.`, `..`
   - Chứa path separator (`/`, `\`)
   - Là absolute path
   - Bắt đầu với `legacy-id-`
4. **Auto-detect repo** (nếu `repo == None` và `detect_repo` callback available):
   - Gọi `detect_repo(ctx, repo_path)` để detect GitHub repo từ git remote
   - Nếu fail → add warning, không reject (non-fatal)

#### Phase 2: Reviewer Auto-Merge Validation
5. Gọi `validate_reviewer_auto_merge_for_project`:
   - Nếu `autoMerge.enabled == false` → skip
   - Nếu `autoMerge.scope` khác `"looper-only"` → reject với validation error
   - Nếu `get_repository_settings` không available và `require_branch_protection == true` → reject
   - Nếu repo không xác định được → reject
   - Fetch repo settings → validate:
     - Strategy (`squash` | `merge` | `rebase`) được phép bởi repo
     - Repo có `allow_auto_merge == true`
   - Nếu `require_branch_protection == true`:
     - Fetch branch protection cho `base_branch`
     - Validate `protection.enabled == true && protection.has_required_checks == true`

#### Phase 3: Build Metadata & Upsert
6. Build metadata JSON (ordered keys):
   - Extra keys (sorted alphabetically) — từ existing metadata nếu reactivate
   - `repo`: detected/explicit repo or null
   - `worktreeRoot`: from input or preserve existing
   - `normalizedDerivedId`: true nếu ID đã được normalize từ legacy
   - `source`: `"api"` (for AddProject)
7. Upsert `ProjectRecord`:
   - Nếu existing → preserve `created_at`
   - `archived = false`, `base_branch`, `repo_path`, `name`
8. **Side effect**: Nếu reactivate archived project → record mới vẫn giữ `created_at` cũ

#### Phase 4: Discovery
9. **discoverWorktrees**:
   - Nếu `list_worktrees` callback không available → skip (0 discovered)
   - Gọi `list_worktrees(repo_path)` → list git worktrees
   - Filter: bỏ bare repos, bỏ worktrees không có branch name
   - Với mỗi worktree:
     - Kiểm tra existing trong DB theo `(project_id, branch)`
     - BaseBranch: worktree branch → project base_branch → existing base_branch
     - HeadSHA: từ git output → existing record
     - `status = "active"`
     - Upsert `WorktreeRecord`
   - Nếu có lỗi → log warning, không reject

10. **discoverPullRequests**:
    - Nếu `snapshot_mode == Off` hoặc repo không xác định → skip
    - Nếu `list_open_pull_requests` không available → skip
    - Gọi `list_open_pull_requests(repo, cwd, limit=1000, timeout=15s)`
    - Filter: bỏ draft PRs, bỏ non-open state
    - Với mỗi PR:
      - Nếu `snapshot_mode == Async` và async snapshot queue enabled:
        - Enqueue snapshot: tạo `QueueItemRecord` với type `"snapshot"`, dedupe key `"snapshot:{project_id}:{repo}:{pr_number}"`
        - Skip nếu đã có active queue item với cùng dedupe key
        - Priority: `QueuePrioritySnapshot`
      - Nếu `snapshot_mode == Full`:
        - Gọi `capture_pull_request_snapshot` callback
        - Nếu fail (trừ context.Canceled) → log warning, continue
        - Nếu `context.Canceled` → propagate error ngay lập tức
        - Upsert snapshot record
      - Nếu async mode nhưng queue bị disabled → fallback sang Full + add warning
    - Nếu có lỗi list PRs → log warning, không reject

**Errors (reject — không tạo project):**
- ProjectID collides with existing explicit project
- Invalid project ID format
- Reviewer auto-merge validation failure
- Context cancelled during snapshot (propagated)

**Warnings (non-fatal, project vẫn được tạo):**
- Could not detect GitHub repo
- Could not discover worktrees
- Could not discover pull requests
- Could not snapshot individual PR (non-cancellation)
- Async mode fallback to full

### 3.2 RemoveProject

**Input:**
```rust
fn RemoveProject(identifier: &str) -> Result<ProjectRecord>
```

**Behavior:**
1. Validate identifier không empty
2. **Resolve project**:
   - Try: `GetByID(identifier)`
   - Nếu tìm thấy và không archived → dùng result
   - Nếu tìm thấy nhưng archived → `ProjectNotFoundError`
   - Nếu không tìm thấy → Try: match theo `name` (case-insensitive, trimmed)
     - Nếu match nhiều hơn 1 → `AmbiguousProjectIdentifierError`
3. **Reject config-managed projects**:
   - Parse `metadata_json.source`
   - Nếu `source == "config"` → `ProjectValidationError`: project is managed by config
4. **Transaction**:
   - Archive project: `UPDATE projects SET archived=1, updated_at=now WHERE id=? AND archived=0`
   - Nếu archive không ảnh hưởng row nào → `ProjectNotFoundError`
   - Terminate all active loops: `repos.Loops.TerminateByProject(project_id, now)`
     - Target statuses: `idle, queued, running, paused, waiting, failed, interrupted`
     - Set `status = terminated`, `next_run_at = NULL`
   - Cancel all active queue items: `repos.Queue.CancelByProject(project_id, now, "project archived")`
     - Target statuses: `queued, running, failed, manual_intervention`
5. Return project record với `archived = true`

**Errors:**
- Project not found (by ID hoặc name)
- Ambiguous project identifier
- Project is managed by config (cannot remove via CLI)
- Context cancelled

### 3.3 List

**Behavior:**
1. `repos.Projects.List()` → tất cả projects
2. Filter: chỉ trả về projects với `archived == false`
3. Sorted by `updated_at DESC`

### 3.4 SyncConfigured

**Input:**
```rust
fn SyncConfigured(cfg: Config, now: DateTime<Utc>) -> Result<()>
```

**Behavior:**
1. Với mỗi project trong `cfg.projects`:
   a. **Upsert or create**:
      - Check existing theo `project.id`
      - Detect repo (nếu callback available)
        - Nếu detect fail: try preserve existing repo metadata nếu `repo_path` không đổi → log warning
        - Nếu detect thành công nhưng empty → preserve existing repo metadata nếu `repo_path` không đổi
   b. **Build metadata JSON** (ordered):
      - Preserve existing extra keys (trừ `repo`, `worktreeRoot`, `source`)
      - `repo`: detected or existing or null
      - `worktreeRoot`: từ config hoặc existing
      - `source = "config"`
   c. **Validate reviewer auto-merge** (identical logic với AddProject)
   d. Upsert ProjectRecord với `archived = false`, preserve `created_at` nếu existing
2. **Lưu ý quan trọng**: Không xóa projects không có trong config — chỉ upsert những project được liệt kê

**Errors:**
- Detection failure không có existing metadata để fallback
- Reviewer auto-merge validation failure

### 3.5 discoverWorktrees

**Behavior (internal helper, không phải public API):**

```rust
fn discover_worktrees(project: ProjectRecord, now_iso: &str, warnings: &mut Vec<String>) -> Result<usize>
```

1. Nếu `list_worktrees` callback không available → return 0
2. Gọi `list_worktrees(project.repo_path)` → `Vec<WorktreeListEntry>`
3. Filter: bỏ bare repos, bỏ entries không có branch name
4. Với mỗi worktree:
   - Check existing: `GetByBranch(project_id, branch)`
   - BaseBranch priority: existing.base_branch > project.base_branch > worktree.branch
   - HeadSHA: từ git output > existing.head_sha
   - Status: `"active"`
   - Nếu existing ID available → preserve; nếu không → sinh ID mới (random hex hoặc timestamp fallback)
   - Upsert WorktreeRecord
5. Nếu list_worktrees fail → log warning, return 0
6. Return số lượng worktrees đã upsert

### 3.6 discoverPullRequests

**Behavior (internal helper):**

```rust
fn discover_pull_requests(
    project: ProjectRecord,
    repo: Option<&str>,
    mode: SnapshotMode,
    warnings: &mut Vec<String>,
) -> Result<(usize, usize, usize)>  // (discovered, pending_snapshots, captured_snapshots)
```

1. Nếu `mode == Off` hoặc repo không determined → return (0, 0, 0)
2. Nếu `mode == Async` nhưng async snapshot queue disabled → fallback sang `Full` + add warning
3. Gọi `list_open_pull_requests` (timeout 15s, limit 1000)
4. Filter: non-draft, open state
5. Với mỗi PR:
   - Nếu `mode == Async` → enqueue snapshot job (dedupe by `snapshot:{project_id}:{repo}:{pr_number}`)
   - Nếu `mode == Full` → capture snapshot trực tiếp, upsert record
     - Nếu context cancelled → propagate
     - Nếu lỗi khác → log warning, continue
6. Return counts

### 3.7 Reviewer Auto-Merge Validation

**Behavior:**

```rust
fn validate_reviewer_auto_merge_for_project(
    project_id: &str,
    repo: Option<&str>,
    base_branch: &str,
    cfg: &Config,
) -> Result<()>
```

1. Lấy `roles.reviewer.autoMerge` config cho project
2. Nếu `auto_merge.enabled == false` → OK
3. Validate `auto_merge.scope` phải là `"looper-only"` — các scope khác unsupported
4. Nếu `get_repository_settings` không available → reject
5. Nếu repo không xác định → reject
6. Fetch repo settings:
   - Validate strategy được cho phép: `RepoSettings { allow_squash_merge, allow_merge_commit, allow_rebase_merge }`
   - Nếu `repo.allow_auto_merge == false` → reject
7. Nếu `require_branch_protection == true`:
   - Nếu `get_branch_protection` không available → reject
   - Xác định branch: `base_branch` → `cfg.defaults.base_branch`
   - Fetch branch protection
   - Validate `protection.enabled && protection.has_required_checks`

**Error message format:**
```
reviewer auto-merge enabled on {repo} but {failure}; disable roles.reviewer.autoMerge.enabled or fix repo settings
```

---

## 4. Event Log Integration

Cả LoopService và RunService đều ghi event log qua `eventlog.Append()`:

### Event types được emit

| Operation | Event Type | Entity | Payload |
|-----------|-----------|--------|---------|
| StartRun | `loop.started` | loop | `{status: "running"}` |
| StartRun | `run.started` | run | `{currentStep, lastCompletedStep}` |
| RecordStep | (tùy input) | run | Tùy payload |
| Complete(success) | `run.completed` | run | `{summary, errorMessage?}` |
| Complete(failure) | `run.failed` | run | `{summary, errorMessage}` |

### Event Log Record Structure

```rust
struct EventLogRecord {
    id:                String,      // random hex
    event_type:        String,      // domain event name
    project_id:        Option<String>,
    loop_id:           Option<String>,
    run_id:            Option<String>,
    entity_type:       Option<String>,  // "loop" | "run"
    entity_id:         Option<String>,
    correlation_id:    Option<String>,
    causation_id:      Option<String>,
    actor_type:        Option<String>,  // default: "system"
    actor_id:          Option<String>,  // default: "looperd"
    actor_display_name: Option<String>, // default: "looperd"
    payload_json:      String,
    created_at:        String,      // ISO format "2006-01-02T15:04:05.000Z"
}
```

---

## 5. Lưu ý triển khai Rust

1. **Transaction pattern**: Các service operations (Create, TransitionStatus, StartRun, RecordStep, Complete, RemoveProject) dùng transaction pattern:
   - Mở transaction
   - Thực hiện reads + writes qua `Repositories` mới từ transaction
   - Commit (rollback tự động qua RAII / Drop)
2. **Retry pattern cho Queue dedupe** (`CreateOrGetActiveByDedupe`):
   - Attempt insert
   - Nếu UNIQUE constraint violation trên dedupe_key và type là `reviewer`|`fixer` → fetch existing active item
   - Return existing thay vì fail
   - Max 2 attempts
3. **Callback injection**: ProjectService injects callbacks cho các IO operations (list worktrees, list PRs, detect repo, capture snapshot) để dễ test. Rust implementation dùng trait objects hoặc function pointers
4. **Time source**: Tất cả services dùng `now: fn() -> DateTime<Utc>` để dễ test. Default fallback đến `Utc::now()`
5. **Event ID generation**: `NewEventID(prefix)` sinh 16-byte random hex với prefix, fallback tới timestamp-based ID nếu hệ thống không có entropy
6. **Snapshot mode**: Mặc định là `Async` (enqueue snapshot job). Fallback xuống `Full` nếu scheduler/queue không available
