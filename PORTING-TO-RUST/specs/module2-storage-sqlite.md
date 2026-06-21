# Module 2: looper-storage (SQLite) — Rust Port Spec

## Source Files
- `internal/storage/repositories.go` — 2720 lines (all record types, repositories, scan helpers)
- `internal/storage/webhook_forwarders.go` — 74 lines
- `internal/storage/webhook_tunnel_hooks.go` — 89 lines
- `internal/storage/migrations/0001_init.sql` — init schema
- `internal/storage/migrations/0002_integrations.sql` — agent_executions, notifications, worktrees
- `internal/storage/migrations/0003_scheduler_queue.sql` — queue_items
- `internal/storage/migrations/0004_worker_project_target.sql` — loops+queue migration for worker→project
- `internal/storage/migrations/0005_planner_issue_target.sql` — target_type 'issue' support
- `internal/storage/migrations/0006_loop_seq_handles.sql` — seq column, counters table
- `internal/storage/migrations/0007_agent_execution_run_index.sql` — index
- `internal/storage/migrations/0008_one_running_run_per_loop.sql` — unique partial index
- `internal/storage/migrations/0009_runs_latest_created_at_index.sql` — index update
- `internal/storage/migrations/0010_agent_native_resume.sql` — native_resume columns
- `internal/storage/migrations/0011_sweeper_cases_proposals.sql` — sweeper tables (now dropped in 0017)
- `internal/storage/migrations/0012_sweeper_proposal_raw_result.sql` — column add
- `internal/storage/migrations/0013_active_queue_dedupe.sql` — unique partial index
- `internal/storage/migrations/0014_webhook_forwarders.sql` — webhook_forwarders table
- `internal/storage/migrations/0015_webhook_tunnel_hooks.sql` — webhook_tunnel_hooks table
- `internal/storage/migrations/0016_queue_infinite_retry_attempts.sql` — max_attempts default -1
- `internal/storage/migrations/0017_remove_sweeper_storage.sql` — drops sweeper tables + queue items

---

## 1. Querier Interface

```rust
// In Go: sqliteQuerier interface
trait SqliteQuerier {
    fn exec_context(&self, ctx: &Context, sql: &str, args: &[DynValue]) -> Result<SqlResult, Error>;
    fn query_context(&self, ctx: &Context, sql: &str, args: &[DynValue]) -> Result<Rows, Error>;
    fn query_row_context(&self, ctx: &Context, sql: &str, args: &[DynValue]) -> Result<Row, Error>;
}
```

---

## 2. REPOSITORY CONTAINER

```rust
struct Repositories {
    projects: ProjectsRepository,
    loops: LoopsRepository,
    runs: RunsRepository,
    agent_executions: AgentExecutionsRepository,
    pull_request_snapshots: PullRequestSnapshotsRepository,
    events: EventsRepository,
    locks: LocksRepository,
    queue: QueueRepository,
    notifications: NotificationsRepository,
    worktrees: WorktreesRepository,
    webhook_forwarders: WebhookForwardersRepository,
    webhook_tunnel_hooks: WebhookTunnelHooksRepository,
}

fn new_repositories(q: impl SqliteQuerier) -> Repositories
```

---

## 3. RECORD TYPES

### ProjectRecord
```rust
struct ProjectRecord {
    id: String,
    name: String,
    repo_path: String,
    base_branch: Option<String>,
    archived: bool,            // stored as 0/1 INTEGER
    metadata_json: Option<String>,
    created_at: String,        // ISO 8601
    updated_at: String,        // ISO 8601
}
```

### LoopRecord
```rust
struct LoopRecord {
    id: String,
    seq: i64,
    project_id: String,
    r#type: String,            // e.g. "planner", "worker", "reviewer", "fixer"
    target_type: String,       // "project", "pull_request", "issue"
    target_id: Option<String>,
    repo: Option<String>,      // "owner/repo" or "hostname/owner/repo"
    pr_number: Option<i64>,
    status: String,            // "idle","queued","running","paused","waiting","failed","interrupted","terminated","completed","stopped"
    config_json: Option<String>,
    metadata_json: Option<String>,
    last_run_at: Option<String>,
    next_run_at: Option<String>,
    created_at: String,
    updated_at: String,
}
```

### RunRecord
```rust
struct RunRecord {
    id: String,
    loop_id: String,
    status: String,                   // "running","completed","failed","interrupted","cancelled"
    current_step: Option<String>,
    last_completed_step: Option<String>,
    checkpoint_json: Option<String>,  // JSON with resumePolicy etc.
    summary: Option<String>,
    error_message: Option<String>,
    started_at: String,
    last_heartbeat_at: Option<String>,
    ended_at: Option<String>,
    created_at: String,
    updated_at: String,
}
```

### AgentExecutionRecord
```rust
struct AgentExecutionRecord {
    id: String,
    project_id: Option<String>,
    loop_id: Option<String>,
    run_id: Option<String>,
    vendor: String,              // AgentVendor enum
    status: String,              // "running","cancelling","completed","failed","interrupted"
    pid: Option<i64>,
    command_json: Option<String>,
    cwd: Option<String>,
    summary: Option<String>,
    parse_status: Option<String>,
    completion_signal: Option<String>,
    heartbeat_count: i64,
    last_heartbeat_at: Option<String>,
    output_json: Option<String>,
    error_message: Option<String>,
    native_session_id: Option<String>,
    native_resume_mode: Option<String>,
    native_resume_status: Option<String>,
    native_resume_error: Option<String>,
    started_at: String,
    ended_at: Option<String>,
    metadata_json: Option<String>,
    created_at: String,
    updated_at: String,
}
```

### PullRequestSnapshotRecord
```rust
struct PullRequestSnapshotRecord {
    id: String,
    project_id: String,
    repo: String,
    pr_number: i64,
    head_sha: String,
    base_sha: Option<String>,
    title: Option<String>,
    body: Option<String>,
    author: Option<String>,
    diff_ref: Option<String>,          // "gh:pr-diff:<repo>:<pr_number>"
    checks_summary: Option<String>,
    unresolved_thread_count: Option<i64>,
    review_state: Option<String>,
    payload_json: Option<String>,      // {detail: PullRequestDetail, diff: String, diffTruncated?: bool}
    captured_at: String,
    created_at: String,
}
```

### EventLogRecord
```rust
struct EventLogRecord {
    id: String,
    event_type: String,
    project_id: Option<String>,
    loop_id: Option<String>,
    run_id: Option<String>,
    entity_type: Option<String>,
    entity_id: Option<String>,
    correlation_id: Option<String>,
    causation_id: Option<String>,
    actor_type: Option<String>,
    actor_id: Option<String>,
    actor_display_name: Option<String>,
    payload_json: String,
    created_at: String,
}
```

### LockRecord
```rust
struct LockRecord {
    key: String,
    owner: String,
    reason: Option<String>,
    expires_at: String,
    created_at: String,
    updated_at: String,
}
```

### QueueItemRecord
```rust
struct QueueItemRecord {
    id: String,
    project_id: Option<String>,
    loop_id: Option<String>,
    r#type: String,                      // "planner","worker","reviewer","fixer"
    target_type: String,
    target_id: String,
    repo: Option<String>,
    pr_number: Option<i64>,
    dedupe_key: String,
    priority: i64,                        // higher = more urgent
    status: String,                       // "queued","running","completed","failed","cancelled","manual_intervention"
    available_at: String,
    attempts: i64,
    max_attempts: i64,                    // default -1 (infinite), >0 for limit
    claimed_by: Option<String>,
    claimed_at: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
    lock_key: Option<String>,
    payload_json: Option<String>,
    last_error: Option<String>,
    last_error_kind: Option<String>,      // "retryable_transient","retryable_after_resume","non_retryable","manual_intervention"
    created_at: String,
    updated_at: String,
}
```

### QueueStats
```rust
struct QueueStats {
    total_queued: i64,
    eligible_queued: i64,
    blocked_by_terminal_or_paused_loop: i64,
    blocked_by_lock_key: i64,
    blocked_by_reviewer_fixer_dependency: i64,
    scheduled_for_future: i64,
    stale_queued: i64,
}
```

### QueueMarkRetryInput
```rust
struct QueueMarkRetryInput {
    id: String,
    available_at: String,
    attempts: i64,
    error_message: Option<String>,
    error_kind: String,
    updated_at: String,
}
```

### QueueFailInput
```rust
struct QueueFailInput {
    id: String,
    attempts: i64,
    finished_at: String,
    error_message: Option<String>,
    error_kind: String,
    updated_at: String,
}
```

### NotificationRecord
```rust
struct NotificationRecord {
    id: String,
    project_id: Option<String>,
    loop_id: Option<String>,
    run_id: Option<String>,
    entity_type: Option<String>,
    entity_id: Option<String>,
    channel: String,
    level: String,
    title: String,
    subtitle: Option<String>,
    body: String,
    status: String,
    dedupe_key: Option<String>,
    error_message: Option<String>,
    payload_json: Option<String>,
    sent_at: Option<String>,
    created_at: String,
    updated_at: String,
}
```

### WorktreeRecord
```rust
struct WorktreeRecord {
    id: String,
    project_id: String,
    repo_path: String,
    worktree_path: String,
    branch: String,
    base_branch: Option<String>,
    status: String,
    head_sha: Option<String>,
    metadata_json: Option<String>,
    created_at: String,
    updated_at: String,
    cleaned_at: Option<String>,
}
```

### WebhookForwarderRecord
```rust
struct WebhookForwarderRecord {
    repo: String,
    pid: i64,
    process_start: i64,
    fingerprint: String,
    endpoint: String,
    events: String,
    gh_path: String,
    daemon_id: String,
    spawned_at: i64,
    updated_at: i64,
}
```

### WebhookTunnelHookRecord
```rust
struct WebhookTunnelHookRecord {
    repo: String,
    hook_id: i64,
    managed_url: String,
    secret_ref: String,
    last_ping_at: Option<i64>,
    consecutive_disables: i64,
    last_disable_at: Option<i64>,
    orphaned: bool,
    created_at: i64,
    updated_at: i64,
}
```

---

## 4. REPOSITORY METHODS

### ProjectsRepository

```rust
impl ProjectsRepository {
    fn upsert(ctx, record: ProjectRecord) -> Result<()>
    fn get_by_id(ctx, id: &str) -> Result<Option<ProjectRecord>>
    fn list(ctx) -> Result<Vec<ProjectRecord>>
    fn archive(ctx, id: &str, updated_at: &str) -> Result<bool>
}
```

### LoopsRepository

```rust
impl LoopsRepository {
    fn upsert(ctx, record: LoopRecord) -> Result<()>
    fn get_by_id(ctx, id: &str) -> Result<Option<LoopRecord>>
    fn get_by_seq(ctx, seq: i64) -> Result<Option<LoopRecord>>
    fn allocate_seq(ctx) -> Result<i64>       // atomically increments counters value
    fn list(ctx) -> Result<Vec<LoopRecord>>
    fn list_by_statuses(ctx, statuses: &[String]) -> Result<Vec<LoopRecord>>
    fn list_by_ids(ctx, ids: &[String]) -> Result<Vec<LoopRecord>>   // chunks at 900 ids
    fn count_by_type_and_status(ctx) -> Result<HashMap<String, HashMap<String, i64>>>
    fn terminate_by_project(ctx, project_id: &str, updated_at: &str) -> Result<i64>
}
```

### RunsRepository

```rust
impl RunsRepository {
    fn upsert(ctx, record: RunRecord) -> Result<()>
    fn get_by_id(ctx, id: &str) -> Result<Option<RunRecord>>
    fn get_latest_by_loop_id(ctx, loop_id: &str) -> Result<Option<RunRecord>>
    fn list_latest_by_loop_ids(ctx, loop_ids: &[String]) -> Result<Vec<RunRecord>>
    fn list_latest_by_loop_statuses_and_resume_policy(ctx, statuses: &[String], resume_policy: &str) -> Result<Vec<RunRecord>>
    fn has_running_by_loop_id(ctx, loop_id: &str) -> Result<bool>
    fn list(ctx) -> Result<Vec<RunRecord>>
    fn count_by_status(ctx) -> Result<HashMap<String, i64>>
    fn list_since(ctx, since_iso: &str) -> Result<Vec<RunRecord>>
    fn list_by_status(ctx, status: &str) -> Result<Vec<RunRecord>>
    fn list_by_loop(ctx, loop_id: &str) -> Result<Vec<RunRecord>>
}
```

### AgentExecutionsRepository

```rust
const AGENT_EXECUTION_COLUMNS: &str = "id, project_id, loop_id, run_id, vendor, status, pid, command_json, cwd, summary, parse_status, completion_signal, heartbeat_count, last_heartbeat_at, output_json, error_message, native_session_id, native_resume_mode, native_resume_status, native_resume_error, started_at, ended_at, metadata_json, created_at, updated_at";

impl AgentExecutionsRepository {
    fn upsert(ctx, record: AgentExecutionRecord) -> Result<()>
    fn get_by_id(ctx, id: &str) -> Result<Option<AgentExecutionRecord>>
    fn get_latest_by_run_id(ctx, run_id: &str) -> Result<Option<AgentExecutionRecord>>
    fn get_latest_active_by_run_id(ctx, run_id: &str) -> Result<Option<AgentExecutionRecord>>
    fn get_latest_by_loop_id(ctx, loop_id: &str) -> Result<Option<AgentExecutionRecord>>
    fn list_active(ctx) -> Result<Vec<AgentExecutionRecord>>
    fn list(ctx) -> Result<Vec<AgentExecutionRecord>>
    fn list_since(ctx, since_iso: &str) -> Result<Vec<AgentExecutionRecord>>
}
```

### PullRequestSnapshotsRepository

```rust
impl PullRequestSnapshotsRepository {
    fn upsert(ctx, record: PullRequestSnapshotRecord) -> Result<()>
    fn list(ctx) -> Result<Vec<PullRequestSnapshotRecord>>
    fn get_latest(ctx, repo: &str, pr_number: i64) -> Result<Option<PullRequestSnapshotRecord>>
    fn get_latest_by_project(ctx, project_id: &str, repo: &str, pr_number: i64) -> Result<Option<PullRequestSnapshotRecord>>
}
```

### EventsRepository

```rust
impl EventsRepository {
    fn append(ctx, record: EventLogRecord) -> Result<()>
    fn list(ctx, limit: i64) -> Result<Vec<EventLogRecord>>
    fn list_since(ctx, since_iso: &str) -> Result<Vec<EventLogRecord>>
    fn list_by_entity(ctx, entity_type: &str, entity_id: &str) -> Result<Vec<EventLogRecord>>
}
```

### LocksRepository

```rust
impl LocksRepository {
    fn set_now(&mut self, now: fn() -> Instant)   // for testing
    fn acquire(ctx, record: LockRecord) -> Result<bool>   // INSERT OR UPDATE WHERE expires_at <= now
    fn release(ctx, key: &str) -> Result<()>      // DELETE
    fn get(ctx, key: &str) -> Result<Option<LockRecord>>
    fn refresh(ctx, record: LockRecord) -> Result<bool>   // UPDATE WHERE key=key AND owner=owner
    fn list_expired(ctx, now_iso: &str) -> Result<Vec<LockRecord>>
}
```

### QueueRepository

```rust
const QUEUE_LONG_TERM_RETRY_ATTEMPT_THRESHOLD: i64 = 5;

impl QueueRepository {
    fn create_or_get_active_by_dedupe(ctx, record: QueueItemRecord) -> Result<(QueueItemRecord, bool)>
    fn upsert_active_by_dedupe_or_get_existing(ctx, record: QueueItemRecord) -> Result<(QueueItemRecord, bool)>
    fn upsert(ctx, record: QueueItemRecord) -> Result<()>
    fn get_by_id(ctx, id: &str) -> Result<Option<QueueItemRecord>>
    fn get_latest_by_loop_id(ctx, loop_id: &str) -> Result<Option<QueueItemRecord>>
    fn list(ctx) -> Result<Vec<QueueItemRecord>>
    fn list_by_statuses(ctx, statuses: &[String]) -> Result<Vec<QueueItemRecord>>
    fn list_latest_by_loop_statuses(ctx, statuses: &[String]) -> Result<Vec<QueueItemRecord>>
    fn count_by_all_statuses(ctx) -> Result<HashMap<String, i64>>
    fn list_queued(ctx, limit: i64) -> Result<Vec<QueueItemRecord>>
    fn count_by_status(ctx, status: &str) -> Result<i64>
    fn count_active_by_loop_id(ctx, loop_id: &str) -> Result<i64>
    fn count_by_loop_id_and_status(ctx, loop_id: &str, status: &str) -> Result<i64>
    fn find_active_by_dedupe(ctx, dedupe_key: &str) -> Result<Option<QueueItemRecord>>
    fn find_active_by_loop_id(ctx, loop_id: &str) -> Result<Option<QueueItemRecord>>
    fn list_scheduled(ctx, now_iso: &str, limit: i64) -> Result<Vec<QueueItemRecord>>
    fn stats(ctx, now_iso: &str) -> Result<QueueStats>
    fn cleanup_stale_queued(ctx, finished_at: &str, reason: &str) -> Result<i64>
    fn claim_next(ctx, now_iso: &str, claimed_by: &str) -> Result<Option<QueueItemRecord>>
    fn claim_next_non_long_term_retry(ctx, now_iso: &str, claimed_by: &str) -> Result<Option<QueueItemRecord>>
    fn claim_next_long_term_retry(ctx, now_iso: &str, claimed_by: &str) -> Result<Option<QueueItemRecord>>
    fn claim_next_of_type(ctx, now_iso: &str, claimed_by: &str, queue_type: &str) -> Result<Option<QueueItemRecord>>
    fn complete(ctx, id: &str, finished_at: &str) -> Result<()>     // status → 'completed'
    fn update_lock_key(ctx, id: &str, lock_key: &str, updated_at: &str) -> Result<()>
    fn mark_retry(ctx, input: QueueMarkRetryInput) -> Result<()>     // status → 'queued'
    fn fail(ctx, input: QueueFailInput) -> Result<()>                // status → 'manual_intervention'
    fn requeue_running_by_loop(ctx, loop_id: &str, queued_at: &str) -> Result<i64>
    fn requeue_latest_cancelled_by_loop(ctx, loop_id: &str, queued_at: &str) -> Result<i64>
    fn requeue_latest_failed_by_loop(ctx, loop_id: &str, queued_at: &str) -> Result<i64>
    fn requeue_failed_by_id(ctx, loop_id: &str, queue_id: &str, queued_at: &str) -> Result<i64>
    fn requeue_failed_by_id_with_attempts(ctx, loop_id: &str, queue_id: &str, queued_at: &str, attempts: i64) -> Result<i64>
    fn cancel_by_loop(ctx, loop_id: &str, finished_at: &str, reason: Option<&str>) -> Result<i64>
    fn cancel_by_project(ctx, project_id: &str, finished_at: &str, reason: Option<&str>) -> Result<i64>
    fn cancel_active_by_loop_except(ctx, loop_id: &str, keep_id: &str, finished_at: &str, reason: Option<&str>) -> Result<i64>
}
```

### NotificationsRepository

```rust
impl NotificationsRepository {
    fn upsert(ctx, record: NotificationRecord) -> Result<()>     // ON CONFLICT(id) DO UPDATE
    fn get_by_id(ctx, id: &str) -> Result<Option<NotificationRecord>>
    fn list(ctx, limit: i64) -> Result<Vec<NotificationRecord>>
    fn get_latest_by_dedupe(ctx, channel: &str, dedupe_key: &str) -> Result<Option<NotificationRecord>>
}
```

### WorktreesRepository

```rust
impl WorktreesRepository {
    fn upsert(ctx, record: WorktreeRecord) -> Result<()>
    fn get_by_id(ctx, id: &str) -> Result<Option<WorktreeRecord>>
    fn get_by_branch(ctx, project_id: &str, branch: &str) -> Result<Option<WorktreeRecord>>
    fn list_by_project(ctx, project_id: &str) -> Result<Vec<WorktreeRecord>>
    fn list_cleanup_candidates(ctx, limit: i32) -> Result<Vec<WorktreeRecord>>
    fn list_active(ctx) -> Result<Vec<WorktreeRecord>>
    fn touch_cleanup_attempt(ctx, id: &str, updated_at: &str) -> Result<()>
}
```

### WebhookForwardersRepository

```rust
impl WebhookForwardersRepository {
    fn list(ctx) -> Result<Vec<WebhookForwarderRecord>>
    fn upsert(ctx, record: WebhookForwarderRecord) -> Result<()>  // ON CONFLICT(repo) DO UPDATE
    fn delete(ctx, repo: &str) -> Result<()>
}
```

### WebhookTunnelHooksRepository

```rust
impl WebhookTunnelHooksRepository {
    fn list(ctx) -> Result<Vec<WebhookTunnelHookRecord>>
    fn get(ctx, repo: &str) -> Result<(WebhookTunnelHookRecord, bool)>
    fn upsert(ctx, record: WebhookTunnelHookRecord) -> Result<()>
    fn mark_orphaned(ctx, repo: &str, orphaned: bool, updated_at: i64) -> Result<()>
    fn delete(ctx, repo: &str) -> Result<()>
    fn update_ping(ctx, repo: &str, at: i64) -> Result<()>
}
```

---

## 5. DATABASE SCHEMA (Final state after all migrations)

```sql
-- schema_migrations (tracking)
CREATE TABLE IF NOT EXISTS schema_migrations (
  id TEXT PRIMARY KEY,
  applied_at TEXT NOT NULL
);

-- projects
CREATE TABLE IF NOT EXISTS projects (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  repo_path TEXT NOT NULL,
  base_branch TEXT,
  archived INTEGER NOT NULL DEFAULT 0,
  metadata_json TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  CHECK (archived IN (0, 1))
);
CREATE INDEX IF NOT EXISTS idx_projects_archived ON projects (archived);

-- loops (final: v4 with seq)
CREATE TABLE IF NOT EXISTS loops (
  id TEXT PRIMARY KEY,
  seq INTEGER NOT NULL,
  project_id TEXT NOT NULL,
  type TEXT NOT NULL,
  target_type TEXT NOT NULL,
  target_id TEXT,
  repo TEXT,
  pr_number INTEGER,
  status TEXT NOT NULL,
  config_json TEXT,
  metadata_json TEXT,
  last_run_at TEXT,
  next_run_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (project_id) REFERENCES projects (id) ON DELETE CASCADE,
  CHECK (target_type IN ('project', 'pull_request', 'issue')),
  CHECK (pr_number IS NULL OR pr_number > 0)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_loops_seq ON loops (seq);
CREATE INDEX IF NOT EXISTS idx_loops_status ON loops (status);
CREATE INDEX IF NOT EXISTS idx_loops_target ON loops (target_type, target_id);
CREATE INDEX IF NOT EXISTS idx_loops_repo_pr ON loops (repo, pr_number);
CREATE INDEX IF NOT EXISTS idx_loops_next_run_at ON loops (next_run_at);

-- counters (for auto-increment in SQLite)
CREATE TABLE IF NOT EXISTS counters (
  name TEXT PRIMARY KEY,
  value INTEGER NOT NULL
);

-- runs
CREATE TABLE IF NOT EXISTS runs (
  id TEXT PRIMARY KEY,
  loop_id TEXT NOT NULL,
  status TEXT NOT NULL,
  current_step TEXT,
  last_completed_step TEXT,
  checkpoint_json TEXT,
  summary TEXT,
  error_message TEXT,
  started_at TEXT NOT NULL,
  last_heartbeat_at TEXT,
  ended_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (loop_id) REFERENCES loops (id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_runs_loop_id_started_at ON runs (loop_id, started_at DESC, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_runs_status ON runs (status);
CREATE UNIQUE INDEX IF NOT EXISTS idx_runs_one_running_per_loop ON runs (loop_id) WHERE status = 'running';

-- locks
CREATE TABLE IF NOT EXISTS locks (
  key TEXT PRIMARY KEY,
  owner TEXT NOT NULL,
  reason TEXT,
  expires_at TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_locks_expires_at ON locks (expires_at);

-- event_logs
CREATE TABLE IF NOT EXISTS event_logs (
  id TEXT PRIMARY KEY,
  event_type TEXT NOT NULL,
  project_id TEXT,
  loop_id TEXT,
  run_id TEXT,
  entity_type TEXT,
  entity_id TEXT,
  correlation_id TEXT,
  causation_id TEXT,
  actor_type TEXT,
  actor_id TEXT,
  actor_display_name TEXT,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY (project_id) REFERENCES projects (id) ON DELETE SET NULL,
  FOREIGN KEY (loop_id) REFERENCES loops (id) ON DELETE SET NULL,
  FOREIGN KEY (run_id) REFERENCES runs (id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_event_logs_entity_created_at ON event_logs (entity_type, entity_id, created_at);
CREATE INDEX IF NOT EXISTS idx_event_logs_type_created_at ON event_logs (event_type, created_at);

-- pull_request_snapshots
CREATE TABLE IF NOT EXISTS pull_request_snapshots (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  repo TEXT NOT NULL,
  pr_number INTEGER NOT NULL,
  head_sha TEXT NOT NULL,
  base_sha TEXT,
  title TEXT,
  body TEXT,
  author TEXT,
  diff_ref TEXT,
  checks_summary TEXT,
  unresolved_thread_count INTEGER,
  review_state TEXT,
  payload_json TEXT,
  captured_at TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY (project_id) REFERENCES projects (id) ON DELETE CASCADE,
  CHECK (pr_number > 0)
);
CREATE INDEX IF NOT EXISTS idx_pull_request_snapshots_repo_pr ON pull_request_snapshots (repo, pr_number, captured_at DESC);

-- agent_executions (final: v2 + native_resume columns)
CREATE TABLE IF NOT EXISTS agent_executions (
  id TEXT PRIMARY KEY,
  project_id TEXT,
  loop_id TEXT,
  run_id TEXT,
  vendor TEXT NOT NULL,
  status TEXT NOT NULL,
  pid INTEGER,
  command_json TEXT,
  cwd TEXT,
  summary TEXT,
  parse_status TEXT,
  completion_signal TEXT,
  heartbeat_count INTEGER NOT NULL DEFAULT 0,
  last_heartbeat_at TEXT,
  output_json TEXT,
  error_message TEXT,
  native_session_id TEXT,
  native_resume_mode TEXT,
  native_resume_status TEXT,
  native_resume_error TEXT,
  started_at TEXT NOT NULL,
  ended_at TEXT,
  metadata_json TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (project_id) REFERENCES projects (id) ON DELETE SET NULL,
  FOREIGN KEY (loop_id) REFERENCES loops (id) ON DELETE SET NULL,
  FOREIGN KEY (run_id) REFERENCES runs (id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_agent_executions_status ON agent_executions (status);
CREATE INDEX IF NOT EXISTS idx_agent_executions_run ON agent_executions (run_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_executions_loop_native_resume ON agent_executions (loop_id, native_session_id, started_at DESC);

-- notifications
CREATE TABLE IF NOT EXISTS notifications (
  id TEXT PRIMARY KEY,
  project_id TEXT,
  loop_id TEXT,
  run_id TEXT,
  entity_type TEXT,
  entity_id TEXT,
  channel TEXT NOT NULL,
  level TEXT NOT NULL,
  title TEXT NOT NULL,
  subtitle TEXT,
  body TEXT NOT NULL,
  status TEXT NOT NULL,
  dedupe_key TEXT,
  error_message TEXT,
  payload_json TEXT,
  sent_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (project_id) REFERENCES projects (id) ON DELETE SET NULL,
  FOREIGN KEY (loop_id) REFERENCES loops (id) ON DELETE SET NULL,
  FOREIGN KEY (run_id) REFERENCES runs (id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_notifications_entity_created_at ON notifications (entity_type, entity_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notifications_dedupe ON notifications (channel, dedupe_key, created_at DESC);

-- worktrees (final: v2 with nullable base_branch)
CREATE TABLE IF NOT EXISTS worktrees (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  repo_path TEXT NOT NULL,
  worktree_path TEXT NOT NULL,
  branch TEXT NOT NULL,
  base_branch TEXT,
  status TEXT NOT NULL,
  head_sha TEXT,
  metadata_json TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  cleaned_at TEXT,
  FOREIGN KEY (project_id) REFERENCES projects (id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_worktrees_project_status ON worktrees (project_id, status, updated_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_worktrees_project_branch ON worktrees (project_id, branch);
CREATE UNIQUE INDEX IF NOT EXISTS idx_worktrees_path ON worktrees (worktree_path);

-- queue_items (final: v3 with max_attempts default -1)
CREATE TABLE IF NOT EXISTS queue_items (
  id TEXT PRIMARY KEY,
  project_id TEXT,
  loop_id TEXT,
  type TEXT NOT NULL,
  target_type TEXT NOT NULL,
  target_id TEXT NOT NULL,
  repo TEXT,
  pr_number INTEGER,
  dedupe_key TEXT NOT NULL,
  priority INTEGER NOT NULL,
  status TEXT NOT NULL,
  available_at TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  max_attempts INTEGER NOT NULL DEFAULT -1,
  claimed_by TEXT,
  claimed_at TEXT,
  started_at TEXT,
  finished_at TEXT,
  lock_key TEXT,
  payload_json TEXT,
  last_error TEXT,
  last_error_kind TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (project_id) REFERENCES projects (id) ON DELETE CASCADE,
  FOREIGN KEY (loop_id) REFERENCES loops (id) ON DELETE CASCADE,
  CHECK (pr_number IS NULL OR pr_number > 0),
  CHECK (priority > 0),
  CHECK (attempts >= 0),
  CHECK (max_attempts = -1 OR max_attempts > 0),
  CHECK (status IN ('queued', 'running', 'completed', 'failed', 'cancelled', 'manual_intervention')),
  CHECK (last_error_kind IS NULL OR last_error_kind IN ('retryable_transient', 'retryable_after_resume', 'non_retryable', 'manual_intervention'))
);
CREATE INDEX IF NOT EXISTS idx_queue_items_status_available_priority ON queue_items (status, available_at, priority, created_at);
CREATE INDEX IF NOT EXISTS idx_queue_items_loop_status ON queue_items (loop_id, status, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_queue_items_type_repo_pr_status ON queue_items (type, repo, pr_number, status, available_at);
CREATE INDEX IF NOT EXISTS idx_queue_items_dedupe_status ON queue_items (dedupe_key, status, updated_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_queue_items_one_active_dedupe ON queue_items (dedupe_key) WHERE type IN ('reviewer', 'fixer') AND status IN ('queued', 'running');

-- webhook_forwarders
CREATE TABLE IF NOT EXISTS webhook_forwarders (
  repo TEXT PRIMARY KEY,
  pid INTEGER NOT NULL,
  process_start INTEGER NOT NULL,
  fingerprint TEXT NOT NULL,
  endpoint TEXT NOT NULL,
  events TEXT NOT NULL,
  gh_path TEXT NOT NULL,
  daemon_id TEXT NOT NULL,
  spawned_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

-- webhook_tunnel_hooks
CREATE TABLE IF NOT EXISTS webhook_tunnel_hooks (
  repo TEXT PRIMARY KEY,
  hook_id INTEGER NOT NULL,
  managed_url TEXT NOT NULL,
  secret_ref TEXT NOT NULL,
  last_ping_at INTEGER,
  consecutive_disables INTEGER NOT NULL DEFAULT 0,
  last_disable_at INTEGER,
  orphaned INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
```

---

## 6. KEY QUERY PATTERNS

### Scheduled Queue (highest priority pattern)
```sql
SELECT qi.*
FROM queue_items qi
LEFT JOIN loops l ON l.id = qi.loop_id
LEFT JOIN projects p ON p.id = qi.project_id
WHERE qi.status = 'queued'
    AND qi.available_at <= ?
    AND (qi.project_id IS NULL OR p.archived = 0)
    AND COALESCE(l.status, 'queued') NOT IN ('paused', 'completed', 'failed', 'interrupted', 'terminated', 'stopped')
    AND (
        qi.lock_key IS NULL
        OR NOT EXISTS (
            SELECT 1 FROM queue_items lock_blocker
            WHERE lock_blocker.lock_key = qi.lock_key
                AND lock_blocker.status = 'running'
                AND lock_blocker.id != qi.id
        )
    )
    AND (
        qi.type != 'fixer'
        OR qi.repo IS NULL
        OR qi.pr_number IS NULL
        OR NOT EXISTS (
            SELECT 1 FROM queue_items blocker
            WHERE blocker.type = 'reviewer'
                AND blocker.repo = qi.repo
                AND blocker.pr_number = qi.pr_number
                AND blocker.status IN ('queued', 'running')
                AND blocker.id != qi.id
        )
    )
ORDER BY CASE WHEN qi.attempts >= 5 AND COALESCE(qi.last_error_kind, '') IN ('retryable_transient', 'retryable_after_resume', 'non_retryable') THEN 1 ELSE 0 END ASC,
    qi.priority ASC, qi.available_at ASC, qi.created_at ASC
```

### Claim Next (atomic claim via CTE UPDATE RETURNING)
```sql
WITH candidate AS (
    <scheduled_queue_query> LIMIT 1
)
UPDATE queue_items
SET status = 'running',
    claimed_by = ?,
    claimed_at = ?,
    started_at = COALESCE(started_at, ?),
    updated_at = ?
WHERE id = (SELECT id FROM candidate)
    AND status = 'queued'
RETURNING *
```

### Lock Acquire (atomic conditional update)
```sql
INSERT INTO locks (key, owner, reason, expires_at, created_at, updated_at)
VALUES (?, ?, ?, ?, ?, ?)
ON CONFLICT(key) DO UPDATE SET
    owner=excluded.owner, reason=excluded.reason,
    expires_at=excluded.expires_at, updated_at=excluded.updated_at
WHERE locks.expires_at <= ?
```

### One running run per loop (partial unique index)
```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_runs_one_running_per_loop
    ON runs (loop_id) WHERE status = 'running';
```

One active queue item per dedupe key (reviewer/fixer only)
```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_queue_items_one_active_dedupe
    ON queue_items (dedupe_key)
    WHERE type IN ('reviewer', 'fixer') AND status IN ('queued', 'running');
```

---

## 7. CONSTANTS

```rust
// Time layout — JavaScript ISO format used across storage
const JAVA_SCRIPT_ISO_STRING_LAYOUT: &str = "2006-01-02T15:04:05.000Z";

// Error sentinel
const ERR_QUEUE_ITEM_NOT_ACTIVE: Error = ...;  // "queue item not active"

// SQL utility constants
const SQLITE_MAX_VARIABLES: usize = 900;
const QUEUE_LONG_TERM_RETRY_ATTEMPT_THRESHOLD: i64 = 5;

// Priority constants (from queue_priorities.go — referenced but defined separately)
// These are referenced from queue_priorities.go:
// QueuePriorityPlanner, QueuePriorityReviewer, etc.
```

---

## 8. SCAN HELPERS (SQL row scanning)

All repositories have corresponding `scan*` functions that handle `sql.NullString`/`sql.NullInt64` → `Option<String>`/`Option<i64>` conversions:

```rust
fn nullable_string(value: Option<String>) -> Option<String>
fn nullable_int64(value: Option<i64>) -> Option<i64>
fn bool_to_int(value: bool) -> i32     // 0 or 1
fn sql_placeholders(count: usize) -> String   // "?,?,?"
fn chunk_strings(values: &[String], chunk_size: usize) -> Vec<Vec<String>>
fn is_queue_active_dedupe_constraint_error(err: &Error) -> bool
```
---

## 9. EVENT LOG SERVICE

### 9.1 Purpose

Structured audit log with correlation tracking, used by Projects/Loops/Runs services and all runners. Every state transition and side effect should produce an event log entry.

### 9.2 Data Model

```rust
struct EventLogRecord {
    id:               String,           // "event_{16 hex}"  — crypto random
    event_type:       String,           // e.g. "loop.created", "run.completed"
    project_id:       Option<String>,
    loop_id:          Option<String>,
    run_id:           Option<String>,
    entity_type:      Option<String>,   // e.g. "worktree", "queue_item"
    entity_id:        Option<String>,
    correlation_id:   Option<String>,   // groups related events (e.g. one triage → dispatch → plan session)
    causation_id:     Option<String>,   // points to the event that caused this one
    actor_type:       Option<String>,   // "system" | "user" | "agent"
    actor_id:         Option<String>,   // "looperd" | GitHub login | agent name
    actor_display_name: Option<String>,
    payload_json:     String,           // arbitrary JSON payload, "{}" if empty
    created_at:       String,           // JavaScript ISO format ("2006-01-02T15:04:05.000Z")
}
```

### 9.3 Append Logic (`eventlog.Append`)

```
Input: context, repositories (Events ref), AppendInput

1. Guard: if repositories.Events is nil → return error "events repository is not configured"

2. Payload resolution:
   - If input.PayloadJSON is set → use as-is
   - Else if input.Payload is set → json.Marshal
   - Else → "{}"

3. ID generation:
   - If input.ID is blank → NewEventID("event") → 16 bytes crypto/rand → hex encode → "event_{hex}"
   - Fallback if rand.Read fails: "event_{unix_nano}"

4. Timestamp:
   - If input.CreatedAt is zero → time.Now().UTC()
   - Format: FormatJavaScriptISOString → "{yyyy}-{MM}-{dd}T{HH}:{mm}:{ss}.{mmm}Z"

5. Actor defaults:
   - If actor_type is nil → "system"
   - If actor_id is nil → "looperd"
   - If actor_display_name is nil → "looperd"

6. Persist: repositories.Events.Append(ctx, record)
```

### 9.4 Event ID Format

```rust
fn new_event_id(prefix: &str) -> String {
    if prefix.is_empty() { prefix = "event"; }
    let mut raw = [0u8; 16];
    match getrandom::getrandom(&mut raw) {
        Ok(_) => format!("{}_{}", prefix, hex::encode(raw)),
        Err(_) => format!("{}_{}", prefix, std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH).unwrap().as_nanos()),
    }
}
```

### 9.5 Timestamp Format

```rust
fn format_javascript_iso_string(value: SystemTime) -> String {
    // Go equivalent: value.UTC().Format("2006-01-02T15:04:05.000Z")
    // chrono: Utc.timestamp_nanos().format("%Y-%m-%dT%H:%M:%S.%3fZ")
}
```

### 9.6 Usage Patterns

Events are appended by:
- **Loops service**: `loop.created`, `loop.status_changed` (with from/to status in payload)
- **Runs service**: `run.started`, `run.step_completed`, `run.completed`, `run.failed`
- **Projects service**: `project.created`, `project.config_updated`
- **Coordinator**: `coordinator.triage_completed`, `coordinator.dispatch`, `coordinator.mergewatch_result`
- **Runner (all)**: `runner.step.{step_name}`, `runner.checkpoint_persisted`
- **Webhook**: `webhook.received`, `webhook.tunnel_opened`, `webhook.tunnel_closed`

### 9.7 Query Methods (EventsRepository)

```rust
fn append(ctx, record) -> Result
fn list(ctx, limit: i64) -> Result<Vec<EventLogRecord>>         // newest first
fn list_since(ctx, since_iso: &str) -> Result<Vec<EventLogRecord>>
fn list_by_entity(ctx, entity_type: &str, entity_id: &str) -> Result<Vec<EventLogRecord>>
```

### 9.8 Indexes

```sql
-- Primary lookup by entity (e.g. all events for a specific loop)
CREATE INDEX IF NOT EXISTS idx_event_logs_entity_created_at
    ON event_logs (entity_type, entity_id, created_at);

-- Time-range queries
CREATE INDEX IF NOT EXISTS idx_event_logs_type_created_at
    ON event_logs (event_type, created_at);
```
