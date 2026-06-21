# Module 6: looper-scheduler — Rust Spec (Nang cao)

> Nguon: `internal/runtime/scheduler.go` (1757 dong), `internal/runtime/runtime.go` (3028 dong),
> `internal/storage/repositories.go` (queue section), `internal/storage/queue_priorities.go`,
> `internal/runtime/active_executions.go`, `internal/loops/failureclass/failureclass.go`

---

## 1. Type Definitions

### 1.1 Scheduler Struct

Scheduler la struct chinh chiu trach nhiem tick loop va claim pump. No duoc tao boi `new_scheduler()` va chay boi `start_scheduler_loop()` / `start_claim_loop()`.

```rust
struct Scheduler {
    /// Khoang thoi gian giua cac tick (PollIntervalSeconds)
    tick_interval: Duration,

    /// Channel dung de bao thuc scheduler tick tu ben ngoai
    /// (webhook, CLI trigger, recovery, new queue item)
    /// Capacity = 1, non-blocking send
    wake_ch: Sender<()>,

    /// Channel nhan tín hieu dung scheduler
    stop_ch: Option<Sender<()>>,

    /// Channel rieng cho claim pump (chay moi 1s doc lap)
    claim_wake_ch: Sender<()>,

    /// Ticker sinh su kien tick dinh ky
    ticker: Option<time::Interval>,

    /// Lock claim de tranh race giua main scheduler tick va claim pump
    claim_mu: Arc<Mutex<()>>,

    /// So luong run toi da co the chay dong thoi
    max_concurrent_runs: usize,

    /// Flag cho tung role discovery
    planner_discovery_enabled: bool,
    coordinator_enabled: fn(project_id: &str) -> bool,
    reviewer_discovery_enabled: bool,
    fixer_discovery_enabled: bool,
    worker_discovery_enabled: bool,

    /// Handler map chua cac role runner
    handlers: HandlerMap,

    /// Async runner de dispatch queue item (goroutine tuong duong)
    async_runner: Box<dyn AsyncRunner>,

    /// Stale run reconciliation function
    reconcile_stale_runs: Option<Arc<dyn Fn(Context) -> Result<StaleRunReconcileSummary>>>,

    /// Logger
    logger: Box<dyn Logger>,

    /// Now provider (cho phep mock trong test)
    now: fn() -> DateTime<Utc>,
}
```

### 1.2 ActiveExecutionRegistry

Registry de track cac agent execution con dang chay, cho phep kill execution theo (loopID, runID, executionID).

```rust
struct ActiveExecutionRegistry {
    inner: Arc<Mutex<HashMap<String, Arc<dyn KillableExecution>>>>,
}

trait KillableExecution: Send + Sync {
    fn kill(&self, reason: &str) -> Result<(), Error>;
}

impl ActiveExecutionRegistry {
    /// Dang ky execution moi, tra ve unregister handle
    fn register(
        &self,
        loop_id: &str,
        run_id: &str,
        execution_id: &str,
        execution: Arc<dyn KillableExecution>,
    ) -> Box<dyn FnOnce() + Send>;

    /// Kill execution theo (loopID, runID, executionID)
    fn kill(&self, loop_id: &str, run_id: &str, execution_id: &str, reason: &str)
        -> Result<bool, Error>;
}

// Key format: "{loop_id}\x00{run_id}\x00{execution_id}"
fn active_execution_key(loop_id: &str, run_id: &str, execution_id: &str) -> String;
```

### 1.3 AsyncRunner Trait

Tuong duong `schedulerAsyncRunner` interface trong Go. Dung de dispatch queue item xu ly bat dong bo (fire-and-forget).

```rust
#[async_trait]
trait AsyncRunner: Send + Sync {
    /// Chay mot closure trong async task (tokio spawn tuong duong)
    async fn run<F, Fut>(&self, f: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send;
}

/// Task tracker de cho toan bo task hoan thanh truoc khi shutdown
struct TaskTracker {
    counter: Arc<AtomicUsize>,
    done: Arc<tokio::sync::Notify>,
}

impl AsyncRunner for TaskTracker {
    async fn run<F, Fut>(&self, f: F) {
        self.counter.fetch_add(1, Ordering::SeqCst);
        let counter = self.counter.clone();
        let done = self.done.clone();
        tokio::spawn(async move {
            f().await;
            if counter.fetch_sub(1, Ordering::SeqCst) == 1 {
                done.notify_one();
            }
        });
    }
}
```

### 1.4 HandlerMap

Map tu queue item type sang handler function.

```rust
struct HandlerMap {
    planners: Option<Arc<dyn PlannerScheduler>>,
    coordinators: Option<Arc<dyn CoordinatorScheduler>>,
    reviewers: Option<Arc<dyn ReviewerScheduler>>,
    fixers: Option<Arc<dyn FixerScheduler>>,
    workers: Option<Arc<dyn WorkerScheduler>>,
    snapshotter: Option<Arc<dyn SnapshotScheduler>>,
}
```

Trait definitions cho tung role:

```rust
#[async_trait]
trait PlannerScheduler: Send + Sync {
    async fn discover_issues(&self, ctx: Context, input: PlannerDiscoveryInput)
        -> Result<PlannerDiscoveryResult>;
    async fn process_claimed_queue_item(&self, ctx: Context, item: QueueItemRecord)
        -> Result<PlannerProcessResult>;
}

#[async_trait]
trait CoordinatorScheduler: Send + Sync {
    async fn discover_issues(&self, ctx: Context, input: CoordinatorDiscoveryInput)
        -> Result<CoordinatorDiscoveryResult>;
}

#[async_trait]
trait ReviewerScheduler: Send + Sync {
    async fn discover_pull_requests(&self, ctx: Context, input: ReviewerDiscoveryInput)
        -> Result<ReviewerDiscoveryResult>;
    async fn discover_pull_request(&self, ctx: Context, input: ReviewerTargetedDiscoveryInput)
        -> Result<ReviewerDiscoveryResult>;
    async fn process_claimed_queue_item(&self, ctx: Context, item: QueueItemRecord)
        -> Result<ReviewerProcessResult>;
}

#[async_trait]
trait FixerScheduler: Send + Sync {
    async fn discover_pull_requests(&self, ctx: Context, input: FixerDiscoveryInput)
        -> Result<FixerDiscoveryResult>;
    async fn discover_pull_request(&self, ctx: Context, input: FixerTargetedDiscoveryInput)
        -> Result<FixerDiscoveryResult>;
    async fn discover_pull_requests_for_base_branch_update(
        &self, ctx: Context, input: FixerBaseBranchDiscoveryInput
    ) -> Result<FixerDiscoveryResult>;
    async fn process_claimed_queue_item(&self, ctx: Context, item: QueueItemRecord)
        -> Result<FixerProcessResult>;
}

#[async_trait]
trait WorkerScheduler: Send + Sync {
    async fn discover_issues(&self, ctx: Context, input: WorkerDiscoveryInput)
        -> Result<WorkerDiscoveryResult>;
    async fn process_claimed_queue_item(&self, ctx: Context, item: QueueItemRecord)
        -> Result<WorkerProcessResult>;
}

#[async_trait]
trait SnapshotScheduler: Send + Sync {
    async fn capture_pull_request_snapshot(
        &self, ctx: Context, input: CapturePullRequestSnapshotInput
    ) -> Result<PullRequestSnapshotRecord>;
}
```

### 1.5 QueuePriority Constants

```rust
/// Thu tu uu tien trong claim:
/// Planner (1) > Reviewer (2) = Fixer (2) > Worker (3) > Snapshot (4)
const QUEUE_PRIORITY_PLANNER:  i64 = 1;
const QUEUE_PRIORITY_REVIEWER: i64 = 2;
const QUEUE_PRIORITY_FIXER:    i64 = 2;  // same as reviewer
const QUEUE_PRIORITY_WORKER:   i64 = 3;
const QUEUE_PRIORITY_SNAPSHOT: i64 = 4;
```

### 1.6 LongTermRetryThreshold

```rust
/// So lan retry toi da truoc khi queue item duoc coi la "long-term retry"
/// Khi item co attempts >= threshold va last_error_kind la retryable,
/// no chi duoc claim khi con slot thua sau khi da claim het non-long-term items
const QUEUE_LONG_TERM_RETRY_ATTEMPT_THRESHOLD: i64 = 5;
```

### 1.7 StaleRunReconcileSummary

```rust
struct StaleRunReconcileSummary {
    mode: StaleRunReconcileMode,
    started_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    candidate_runs: u64,
    interrupted_runs: u64,
    loops_requeued: u64,
    queue_items_requeued: u64,
    queue_items_cancelled: u64,
    cleaned_executions: u64,
    skipped_uncertain_runs: u64,
    events_written: u64,
    run_ids: Vec<String>,
    loop_ids: Vec<String>,
    execution_ids: Vec<String>,
}

enum StaleRunReconcileMode {
    Startup,
    Live,
    Manual,
}
```

### 1.8 QueueFailureKind

```rust
enum QueueFailureKind {
    RetryableTransient,      // loi mang, API, timeout -> retry exponential backoff
    RetryableAfterResume,    // co the tiep tuc sau restart -> retry voi resume context
    NonRetryable,            // loi vinh vien, khong retry
    ManualIntervention,      // can nguoi xu ly -> khong retry, chuyen sang manual queue
}
```

### 1.9 FailureBoundary

```rust
enum FailureBoundary {
    GitRemote,          // loi git remote (network, auth)
    GitLocal,           // loi git local (conflict, deterministic)
    GitHubAPI,          // loi GitHub API (rate limit, 500, hoac 400/422)
    ModelProvider,      // loi LLM provider (timeout, quota)
    AgentProcess,       // loi agent process (crash, OOM)
    LocalWorktree,      // worktree dirty, can manual intervention
    Storage,            // loi DB/storage
    Config,             // loi config sai
    Checkpoint,         // loi checkpoint invariant
    Policy,             // policy deny
    Unknown,            // khong xac dinh duoc
}

struct FailureClassificationContext {
    runner: RunnerKind,     // planner, reviewer, fixer, worker
    step: String,           // ten step hien tai
    boundary: FailureBoundary,
    side_effect_state: Option<String>,
}
```

### 1.10 SchedulerConfig (from config types)

```rust
struct SchedulerConfig {
    poll_interval_seconds: u32,          // mac dinh: 30
    max_concurrent_runs: u32,            // mac dinh: 5
    retry_max_attempts: u32,             // mac dinh: 3
    retry_base_delay_ms: u32,            // mac dinh: 1000
    slow_lane_warn_threshold_ms: u32,    // mac dinh: 5000
    discovery_cache_ttl_seconds: u32,    // mac dinh: 60
}
```

---

## 2. Constructor va Lifecycle Functions

### 2.1 new_scheduler()

```rust
fn new_scheduler(
    cfg: SchedulerConfig,
    logger: Box<dyn Logger>,
    now: fn() -> DateTime<Utc>,
    handlers: HandlerMap,
    async_runner: Box<dyn AsyncRunner>,
    reconcile_stale_runs: Option<Arc<dyn Fn(Context) -> Result<StaleRunReconcileSummary>>>,
) -> Scheduler
```

### 2.2 start_scheduler_loop()

Khoi dong scheduler loop chinh. Goi `execute_scheduler_tick()` ngay lan dau, sau do cho wake hoac ticker.

```rust
impl Scheduler {
    fn start(&mut self, ctx: Context) {
        let poll_interval = self.tick_interval;

        // Claim pump chay tren tokio task rieng
        if self.handlers.has_claim_handler() {
            let claim_wake_ch = self.claim_wake_ch.clone();
            tokio::spawn(async move {
                Scheduler::run_claim_loop(ctx.clone(), stop_ch, claim_wake_ch).await;
            });
        }

        // Main scheduler loop
        tokio::spawn(async move {
            self.execute_scheduler_tick(ctx.clone()).await;

            if poll_interval.is_zero() {
                // Chi wake-driven (pollIntervalSeconds == 0)
                loop {
                    tokio::select! {
                        _ = stop_ch.closed() => break,
                        _ = wake_ch.recv() => self.execute_scheduler_tick(ctx.clone()).await,
                    }
                }
            } else {
                let mut ticker = tokio::time::interval(poll_interval);
                // Bo qua delay lan dau (tick ngay)
                ticker.tick().await;

                loop {
                    tokio::select! {
                        _ = stop_ch.closed() => break,
                        _ = wake_ch.recv() => self.execute_scheduler_tick(ctx.clone()).await,
                        _ = ticker.tick() => self.execute_scheduler_tick(ctx.clone()).await,
                    }
                }
            }
        });
    }
}
```

### 2.3 start_claim_loop()

Claim pump doc lap chay moi 1 giay.

```rust
impl Scheduler {
    const CLAIM_PUMP_INTERVAL: Duration = Duration::from_secs(1);

    async fn run_claim_loop(
        ctx: Context,
        stop_ch: Receiver<()>,
        wake_ch: Receiver<()>,
    ) {
        // Goi claim pass ngay lan dau
        self.execute_claim_pass(ctx.clone()).await;

        let mut ticker = tokio::time::interval(CLAIM_PUMP_INTERVAL);

        loop {
            tokio::select! {
                _ = stop_ch.closed() => return,
                _ = wake_ch.recv() => self.execute_claim_pass(ctx.clone()).await,
                _ = ticker.tick() => self.execute_claim_pass(ctx.clone()).await,
            }
        }
    }
}
```

### 2.4 Trigger Methods

```rust
impl Scheduler {
    /// Gui tin hieu wake cho ca main tick loop va claim pump
    fn trigger_tick(&self) {
        let _ = self.wake_ch.try_send(());
        let _ = self.claim_wake_ch.try_send(());
    }

    /// Gui tin hieu wake chi cho claim pump
    fn trigger_claim(&self) {
        let _ = self.claim_wake_ch.try_send(());
    }
}
```

### 2.5 Shutdown

```rust
impl Scheduler {
    async fn shutdown(&mut self) {
        if let Some(stop_ch) = self.stop_ch.take() {
            let _ = stop_ch.send(()).await;
        }
        // Doi task tracker hoan thanh (cac async runner)
        // ...
    }
}
```

---

## 3. Complete Tick Flow

### 3.1 execute_scheduler_tick()

```rust
impl Scheduler {
    async fn execute_scheduler_tick(&self, ctx: Context) -> Result<()> {
        let started_at = Instant::now();
        let mut claim_stats = ClaimStats::default();

        // 1. Pre-discovery claim phase
        let (claimed, slots, err) = self.execute_claim_phase(
            ctx.clone(), Phase::PreDiscovery, &HashSet::new(), true
        ).await;
        claim_stats.record(claimed, slots);
        // collect error

        // 2. List projects
        let projects = repos.projects.list(ctx).await?;

        let mut discovery_tick_state = DiscoveryTickState::new();
        let mut project_snapshots: HashMap<String, Arc<DiscoverySnapshot>> = HashMap::new();

        for project in &projects {
            if ctx.is_cancelled() { break; }
            if project.archived { continue; }

            let repo = extract_repo_from_metadata(&project.metadata_json);
            if repo.is_empty() { continue; }

            let snapshot = self.get_or_create_snapshot(
                &project.id, &repo, &mut project_snapshots,
                &discovery_tick_state
            );

            // 2a. Planner discovery + claim
            if let Some(planner) = &self.handlers.planners {
                if self.planner_discovery_enabled {
                    let result = planner.discover_issues(DiscoveryInput {
                        project_id: &project.id,
                        repo: &repo,
                        snapshot: snapshot.clone(),
                    }).await;
                    self.track_runnable_ids(&result.queue_items, &mut discovered_runnable_ids);
                    self.execute_claim_phase(ctx.clone(), Phase::PostPlannerDiscovery, &discovered_runnable_ids, true).await;
                }
            }

            // 2b. Coordinator discovery + claim
            if let Some(coordinator) = &self.handlers.coordinators {
                if self.coordinator_enabled(&project.id) {
                    let result = coordinator.discover_issues(DiscoveryInput {
                        project_id: &project.id,
                        repo: &repo,
                        snapshot: snapshot.clone(),
                    }).await;
                    self.execute_claim_phase(ctx.clone(), Phase::PostCoordinatorDiscovery, &discovered_runnable_ids, true).await;
                }
            }

            // 2c. Reviewer discovery + claim
            if let Some(reviewer) = &self.handlers.reviewers {
                if self.reviewer_discovery_enabled {
                    let result = reviewer.discover_pull_requests(DiscoveryInput {
                        project_id: &project.id,
                        repo: &repo,
                        snapshot: snapshot.clone(),
                    }).await;
                    self.track_runnable_ids(&result.queue_items, &mut discovered_runnable_ids);
                    self.execute_claim_phase(ctx.clone(), Phase::PostReviewerDiscovery, &discovered_runnable_ids, true).await;
                }
            }

            // 2d. Fixer discovery + claim
            if let Some(fixer) = &self.handlers.fixers {
                if self.fixer_discovery_enabled {
                    let result = fixer.discover_pull_requests(DiscoveryInput {
                        project_id: &project.id,
                        repo: &repo,
                        snapshot: snapshot.clone(),
                    }).await;
                    self.track_runnable_ids(&result.queue_items, &mut discovered_runnable_ids);
                    self.execute_claim_phase(ctx.clone(), Phase::PostFixerDiscovery, &discovered_runnable_ids, true).await;
                }
            }

            // 2e. Worker discovery + claim
            if let Some(worker_issues) = &self.handlers.worker_issues_discovery {
                if self.worker_discovery_enabled {
                    let result = worker_issues.discover_issues(DiscoveryInput {
                        project_id: &project.id,
                        repo: &repo,
                        snapshot: snapshot.clone(),
                    }).await;
                    self.track_runnable_ids(&result.queue_items, &mut discovered_runnable_ids);
                    self.execute_claim_phase(ctx.clone(), Phase::PostWorkerDiscovery, &discovered_runnable_ids, true).await;
                }
            }
        }

        // 3. Post-discovery claim phase
        self.execute_claim_phase(ctx.clone(), Phase::PostDiscovery, &discovered_runnable_ids, true).await;

        // Log summary
        log_tick_summary(&self.logger, started_at, &claim_stats, &errors);
    }
}
```

### 3.2 execute_claim_phase()

```rust
impl Scheduler {
    async fn execute_claim_phase(
        &self,
        ctx: Context,
        phase: ClaimPhase,
        discovered_runnable_ids: &HashSet<String>,
        always_log: bool,
    ) -> (usize, usize, Option<Error>) {
        // Lock claim de tranh race
        let _guard = self.claim_mu.lock().await;
        let start = Instant::now();

        // Compute available slots
        let available = self.compute_available_slots(ctx.clone()).await;

        // Neu full slot, thu reconcile stale runs truoc
        let available = if available == 0 {
            if let Some(reconcile) = &self.reconcile_stale_runs {
                let _ = reconcile(ctx.clone()).await;
                self.compute_available_slots(ctx.clone()).await
            } else {
                0
            }
        } else {
            available
        };

        // Claim items
        let claimed_items = if available > 0 {
            let items = self.claim_and_run(ctx.clone(), available).await;
            // Neu co claimed items thuoc discovered set, request wake
            if !items.is_empty() && !discovered_runnable_ids.is_empty() {
                for item in &items {
                    if discovered_runnable_ids.contains(&item.id) {
                        self.trigger_tick();
                        break;
                    }
                }
            }
            items
        } else {
            Vec::new()
        };

        let claimed = claimed_items.len();
        log_claim_phase(&self.logger, &phase, available, claimed, start);

        (claimed, available, None)
    }
}
```

### 3.3 claim_and_run_scheduled_queue_items()

```rust
impl Scheduler {
    async fn claim_and_run(&self, ctx: Context, available_slots: usize) -> Vec<QueueItemRecord> {
        let now_iso = format_javascript_iso_string(self.now().to_utc());
        let mut queue_items = Vec::with_capacity(available_slots);

        // Bucket 1: Non-long-term retry items (u tien cao)
        for _ in 0..available_slots {
            if ctx.is_cancelled() { break; }
            match self.repos.queue.claim_next_non_long_term_retry(ctx.clone(), &now_iso, "scheduler").await {
                Ok(Some(item)) => queue_items.push(item),
                Ok(None) => break,
                Err(_) => break,
            }
        }

        // Bucket 2: Long-term retry items (fill remaining slots)
        while queue_items.len() < available_slots {
            if ctx.is_cancelled() { break; }
            match self.repos.queue.claim_next_long_term_retry(ctx.clone(), &now_iso, "scheduler").await {
                Ok(Some(item)) => queue_items.push(item),
                Ok(None) => break,
                Err(_) => break,
            }
        }

        // Dispatch all claimed items
        if !queue_items.is_empty() {
            self.dispatch_queue_items(ctx.clone(), &queue_items).await;
        }

        queue_items
    }
}
```

### 3.4 dispatch_queue_items()

```rust
impl Scheduler {
    async fn dispatch_queue_items(&self, ctx: Context, items: &[QueueItemRecord]) {
        for item in items {
            let processor = match self.resolve_processor(item).await {
                Ok(p) => p,
                Err(e) => {
                    // log error
                    continue;
                }
            };

            let item_id = item.id.clone();
            let item_type = item.ty.clone();

            self.async_runner.run(async move {
                match processor.process(ctx).await {
                    Ok(_) => {},
                    Err(e) => {
                        log!("scheduler queue item failed: type={} id={} error={}",
                            item_type, item_id, e);
                    }
                }
            }).await;
        }
    }

    async fn resolve_processor(&self, item: &QueueItemRecord) -> Result<Box<dyn QueueItemProcessor>> {
        match item.ty.as_str() {
            "planner" => {
                let p = self.handlers.planners
                    .ok_or_else(|| anyhow!("planner runner not configured"))?;
                Ok(Box::new(PlannerProcessor::new(p.clone(), item.clone())))
            },
            "reviewer" => {
                let r = self.handlers.reviewers
                    .ok_or_else(|| anyhow!("reviewer runner not configured"))?;
                Ok(Box::new(ReviewerProcessor::new(r.clone(), item.clone())))
            },
            "fixer" => {
                let f = self.handlers.fixers
                    .ok_or_else(|| anyhow!("fixer runner not configured"))?;
                Ok(Box::new(FixerProcessor::new(f.clone(), item.clone())))
            },
            "worker" => {
                let w = self.handlers.workers
                    .ok_or_else(|| anyhow!("worker runner not configured"))?;
                Ok(Box::new(WorkerProcessor::new(w.clone(), item.clone())))
            },
            "snapshot" => {
                Ok(Box::new(SnapshotProcessor::new(item.clone(), self)))
            },
            _ => Err(anyhow!("unsupported queue item type: {}", item.ty)),
        }
    }
}
```

---

## 4. SchedulerConfig Defaults

```rust
impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            poll_interval_seconds: 30,
            max_concurrent_runs: 5,
            retry_max_attempts: 3,
            retry_base_delay_ms: 1000,
            slow_lane_warn_threshold_ms: 5000,
            discovery_cache_ttl_seconds: 60,
        }
    }
}
```

---

## 5. Queue Priority Ordering

Khi claim queue item, thu tu uu tien nhu sau:

```sql
-- Long-term retry items bi day xuong cuoi
-- Priority ASC (1 cao nhat)
-- Cung priority: available_at ASC (cu hon truoc), created_at ASC (cu hon truoc)
ORDER BY
    CASE WHEN qi.attempts >= 5
        AND COALESCE(qi.last_error_kind, '') IN ('retryable_transient', 'retryable_after_resume', 'non_retryable')
    THEN 1 ELSE 0 END ASC,
    qi.priority ASC,
    qi.available_at ASC,
    qi.created_at ASC
```

**Priority order trong thuc te:**
| Priority | Loai | Ghi chu |
|----------|------|---------|
| 1 | Planner | Cao nhat |
| 2 | Reviewer / Fixer | Reviewer va Fixer cung priority |
| 3 | Worker | Thap hon |
| 4 | Snapshot | Thap nhat |

Claim duoc chia lam 2 pass:
1. **Pass 1**: Chi claim items co attempts < 5 hoac khong phai retryable (`ClaimNextNonLongTermRetry`)
   - Claim toi da availableSlots items
   - Dung lai khi het hoac fail
2. **Pass 2**: Chi claim items con lai, attempts >= 5 va retryable (`ClaimNextLongTermRetry`)
   - Fill remaining slots (availableSlots - count(PASS1))
   - Dung lai khi het hoac fail

---

## 6. claim_next() CTE SQL Pattern

Exact SQL pattern cho `ClaimNextNonLongTermRetry` va `ClaimNextLongTermRetry`:

### 6.1 scheduledQueueBaseQuery

```sql
SELECT qi.*
FROM queue_items qi
LEFT JOIN loops l ON l.id = qi.loop_id
LEFT JOIN projects p ON p.id = qi.project_id
WHERE qi.status = 'queued'
    AND qi.available_at <= ?
    AND (qi.project_id IS NULL OR p.archived = 0)
    AND COALESCE(l.status, 'queued') NOT IN (
        'paused', 'completed', 'failed', 'interrupted', 'terminated', 'stopped'
    )
    AND (
        qi.lock_key IS NULL
        OR NOT EXISTS (
            SELECT 1
            FROM queue_items lock_blocker
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
            SELECT 1
            FROM queue_items blocker
            WHERE blocker.type = 'reviewer'
                AND blocker.repo = qi.repo
                AND blocker.pr_number = qi.pr_number
                AND blocker.status IN ('queued', 'running')
                AND blocker.id != qi.id
        )
    )
```

**Giai thich cac dieu kien:**

- `qi.status = 'queued'`: Chi claim item dang cho
- `qi.available_at <= ?`: Chi item da den thoi diem available (respect retry delay)
- `p.archived = 0`: Bo qua project da archived
- `l.status NOT IN (...):`: Chi claim loop con hoat dong (khong pause/fail/complete/...)
- **Lock key**: Neu item co lock_key, dam bao khong co running item khac dang giu lock do
- **Fixer blocking**: Neu fixer claim fixer item, dam bao khong co reviewer item nao cho hoac dang chay cung repo/pr_number

### 6.2 ClaimNextNonLongTermRetry

```sql
WITH candidate AS (
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
            OR qi.repo IS NULL OR qi.pr_number IS NULL
            OR NOT EXISTS (
                SELECT 1 FROM queue_items blocker
                WHERE blocker.type = 'reviewer'
                    AND blocker.repo = qi.repo
                    AND blocker.pr_number = qi.pr_number
                    AND blocker.status IN ('queued', 'running')
                    AND blocker.id != qi.id
            )
        )
        -- NON-LONG-TERM: attempts < 5 OR not retryable kind
        AND NOT (
            qi.attempts >= ?
            AND COALESCE(qi.last_error_kind, '') IN ('retryable_transient', 'retryable_after_resume', 'non_retryable')
        )
    ORDER BY qi.priority ASC, qi.available_at ASC, qi.created_at ASC
    LIMIT 1
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

### 6.3 ClaimNextLongTermRetry

Giong `ClaimNextNonLongTermRetry` nhung filter nguoc lai:

```sql
-- Thay NOT (...) bang (...)
AND (
    qi.attempts >= ?
    AND COALESCE(qi.last_error_kind, '') IN ('retryable_transient', 'retryable_after_resume', 'non_retryable')
)
```

---

## 7. Running Count Computation

```rust
impl Scheduler {
    async fn compute_available_slots(&self, ctx: Context) -> usize {
        let repos = self.repos();

        if self.max_concurrent_runs == 0 {
            return 0;
        }

        let running_count = repos.queue.count_by_status(ctx, "running").await
            .unwrap_or(0);

        if running_count >= self.max_concurrent_runs as i64 {
            0
        } else {
            (self.max_concurrent_runs as i64 - running_count) as usize
        }
    }
}
```

SQL cho `count_by_status`:

```sql
SELECT COUNT(*) FROM queue_items WHERE status = ?
```

---

## 8. Wake Channel Trigger Sources

Scheduler co 2 channels co capacity = 1:
- `wake_ch`: trigger `execute_scheduler_tick()`
- `claim_wake_ch`: trigger `execute_claim_pass()`

Cac nguon trigger:

| Nguon | Channel | Khi nao |
|-------|---------|---------|
| Webhook | wake_ch + claim_wake_ch | Nhan duoc webhook event tu GitHub (issue_comment, pull_request, push,...) |
| CLI trigger | wake_ch + claim_wake_ch | Nguoi dung go `looperd scheduler tick` |
| Recovery pipeline | claim_wake_ch | Sau khi recovery pipeline hoan thanh, claim lai cac item bi interrupted |
| New queue item | wake_ch | `OnQueueItemEnqueued` callback -> `requestWake()` |
| Ticker (dinh ky) | wake_ch | Moi `pollIntervalSeconds` cho main tick |
| Claim ticker | claim_wake_ch | Moi 1 giay cho claim pump |

**Implementation detail (Rust):**
```rust
/// Trigger wake cho ca main tick va claim pump
fn request_scheduler_wake(scheduler: &Scheduler) {
    let _ = scheduler.wake_ch.try_send(());
    let _ = scheduler.claim_wake_ch.try_send(());
}

/// Trigger wake chi cho claim pump
fn request_scheduler_claim(scheduler: &Scheduler) {
    let _ = scheduler.claim_wake_ch.try_send(());
}
```

Channel capacity = 1 va `try_send` (non-blocking) de tranh blocking sender khi channel da day.

---

## 9. Discovery Functions Per Role

### 9.1 Planner Discovery

```rust
async fn planner_discover_issues(
    planner: &dyn PlannerScheduler,
    ctx: Context,
    project_id: &str,
    repo: &str,
    snapshot: Option<Arc<DiscoverySnapshot>>,
) -> Result<PlannerDiscoveryResult> {
    planner.discover_issues(ctx, PlannerDiscoveryInput {
        project_id: project_id.to_string(),
        repo: repo.to_string(),
        snapshot,
    }).await
}
```

**Trigger conditions (trong runner):**
- Project khong archived
- `AutoDiscovery = true` trong project config
- Labels match (label mode: ALL hoac ANY)
- Neu `RequireAssigneeCurrentUser`: issue phai duoc assign cho current user

**Github API calls (qua Gateway):**
- `ListOpenIssues(repo, label: "looper:plan")`
- `GetCurrentUserLogin(repo)` (neu `RequireAssigneeCurrentUser`)
- `AddIssueAssignees(repo, issue, assignee)` (cho manual queue)

**Dedupe Key:**
```
planner:{projectID}:{loopID}:{repo}:{issueNumber}
```

**Lock Key:**
```
issue:{repo}:{number}
```

### 9.2 Reviewer Discovery

```rust
async fn reviewer_discover_pull_requests(
    reviewer: &dyn ReviewerScheduler,
    ctx: Context,
    project_id: &str,
    repo: &str,
    snapshot: Option<Arc<DiscoverySnapshot>>,
) -> Result<ReviewerDiscoveryResult> {
    reviewer.discover_pull_requests(ctx, ReviewerDiscoveryInput {
        project_id: project_id.to_string(),
        repo: repo.to_string(),
        snapshot,
    }).await
}
```

**Trigger conditions:**
- `AutoDiscovery = true`
- Optionally: include drafts, require review request, label matching
- Self-review suppression (tru khi `EnableSelfReview = true`)
- Spec PR: optionally include `looper:spec-reviewing` label

**Github API calls:**
- `ListOpenPullRequests(repo, labels, author, ...)`
- `ListReviewRequestedPullRequests(repo, reviewer)` (neu `RequireReviewRequest`)

**Dedupe Key:**
```
reviewer:{projectID}:{loopID}:{repo}:{prNumber}
```

**Lock Key:**
```
pr:{repo}:{number}
```

**Targeted Discovery (tu webhook):**
- `DiscoverPullRequest(repo, prNumber)` — khi PR duoc tao moi hoặc updated
- `DiscoverPullRequestsForBaseBranchUpdate(repo, baseBranch)` — khi base branch thay doi

### 9.3 Fixer Discovery

```rust
async fn fixer_discover_pull_requests(
    fixer: &dyn FixerScheduler,
    ctx: Context,
    project_id: &str,
    repo: &str,
    snapshot: Option<Arc<DiscoverySnapshot>>,
) -> Result<FixerDiscoveryResult> {
    fixer.discover_pull_requests(ctx, FixerDiscoveryInput {
        project_id: project_id.to_string(),
        repo: repo.to_string(),
        snapshot,
    }).await
}
```

**Trigger conditions:**
- `AutoDiscovery = true`
- Optionally: include drafts, author filter, label matching (label mode)

**Github API calls:**
- `ListOpenPullRequests(repo, author, labels, baseRefName)`
- `ListOpenPullRequests(repo, label: "looper:auto-fix")`
- `GetPullRequestAuthor(repo, prNumber)` (neu author filter)

**Dedupe Key:**
```
fixer:{loopID}
```

**Lock Key:**
```
pr:{repo}:{number}
```

### 9.4 Worker Discovery

```rust
async fn worker_discover_issues(
    worker: &dyn WorkerIssueDiscoveryScheduler,  // trait rieng cho issue discovery
    ctx: Context,
    project_id: &str,
    repo: &str,
    snapshot: Option<Arc<DiscoverySnapshot>>,
) -> Result<WorkerDiscoveryResult> {
    worker.discover_issues(ctx, WorkerDiscoveryInput {
        project_id: project_id.to_string(),
        repo: repo.to_string(),
        snapshot,
    }).await
}
```

**Trigger conditions:**
- `AutoDiscovery = true`
- Label match (default: `looper:worker-ready`)
- Optionally requires current-user assignee

**Github API calls:**
- `ListOpenIssues(repo, label: "looper:worker-ready")`
- `GetCurrentUserLogin(repo)` (neu `RequireAssigneeCurrentUser`)

**Dedupe Key:**
- Issue: `worker:{projectID}:{repo}:{issueNumber}`
- PR: `worker:{projectID}:{repo}:{prNumber}`

**Lock Key:**
- Issue: `issue:{repo}:{number}`
- PR: `pr:{repo}:{number}`

### 9.5 Coordinator Discovery

Coordinator khong phai step-based runner ma la orchestrator chay trong tick discover loop.

```rust
async fn coordinator_discover_issues(
    coordinator: &dyn CoordinatorScheduler,
    ctx: Context,
    project_id: &str,
    repo: &str,
    snapshot: Option<Arc<DiscoverySnapshot>>,
) -> Result<CoordinatorDiscoveryResult> {
    if !coordinator_enabled_for_project(project_id) {
        return Ok(CoordinatorDiscoveryResult::default());
    }
    coordinator.discover_issues(ctx, CoordinatorDiscoveryInput {
        project_id: project_id.to_string(),
        repo: repo.to_string(),
        snapshot,
    }).await
}
```

**Phases cua Coordinator discovery:**
1. **Rate limit**: Rate-limited per project (`shouldRunTick()`)
2. **List issues**: List open issues (up to 100)
3. **Load issues**: Load detail, timeline, triage metadata cho moi issue
4. **Merge Watch Phase**: Re-trigger downstream actions cho PRs vua merged
5. **Dependency Phase**: Build dependency graph (blocked-by chains)
6. **Dispatch Phase**: Assign workers/fixers/reviewers based on labels
7. **Review Assignment Phase**: Add reviewers to PRs
8. **Triage Phase**: Goi LLM de phan loai issues moi

---

## 10. Stale Run Reconciliation

Khi `availableSlots == 0`, scheduler goi `reconcile_stale_runs` truoc khi claim lai.

### 10.1 Reconciliation Flow

```rust
async fn reconcile_stale_runs(ctx: Context, repos: &Repositories, now: DateTime<Utc>, mode: StaleRunReconcileMode)
    -> Result<StaleRunReconcileSummary>
{
    // 1. List all running runs
    let running_runs = repos.runs.list_by_status(ctx.clone(), "running").await?;

    // 2. List active agent executions
    let active_executions = repos.agent_executions.list_active(ctx.clone()).await?;

    // 3. For each running run, evaluate if stale
    for run in &running_runs {
        let decision = evaluate_stale_run_candidate(ctx.clone(), repos, run, &active_executions, now, &mode).await?;
        if !decision.candidate { continue; }
        if decision.uncertain { skip(); continue; }

        // Interrupt run
        let _ = mark_run_interrupted(ctx.clone(), repos, run, now).await;

        // Repair queue state: requeue or cancel queue items
        let _ = repair_queue_items_for_run(ctx.clone(), repos, run, now).await;
    }
}
```

### 10.2 Startup vs Live Mode

| Mode | Stale detection |
|------|----------------|
| `startup` | Tat ca running runs deu la candidates (post-recovery) |
| `live` | Chi runs co stale heartbeats (>30 phut khong heartbeat) |
| `manual` | Giong live — goi tu CLI/API |

---

## 11. Runner Instantiation (buildDefaultSchedulerHandlers)

Day la function khoi tao toan bo runners cho scheduler.

```rust
fn build_scheduler_handlers(
    cfg: &Config,
    logger: Box<dyn Logger>,
    coordinator: Arc<SQLiteCoordinator>,
    repos: Arc<Repositories>,
    git_gateway: Arc<GitGateway>,
    github_gateway: Arc<GitHubGateway>,
    active_executions: Arc<ActiveExecutionRegistry>,
    async_runner: Box<dyn AsyncRunner>,
    request_wake: Box<dyn Fn() + Send + Sync>,
    now: fn() -> DateTime<Utc>,
    reconcile_stale_runs: Option<Arc<dyn Fn(Context) -> Result<StaleRunReconcileSummary>>>,
) -> Result<SchedulerHandlers>
```

**Logic:**

1. Validate dependencies (repos, coordinator, agent vendor)
2. Create notification gateway
3. Create agent executor
4. Instantiate each runner:

**Planner:**
```rust
Planner::new(PlannerOptions {
    db: coordinator.db(),
    repos: repos.clone(),
    github: PlannerGitHubAdapter::new(github_gateway.clone(), stamper.clone()),
    git: PlannerGitAdapter::new(git_gateway.clone(), stamper.clone()),
    agent_executor: PlannerAgentExecutorAdapter::new(agent_executor.clone()),
    logger: logger.new_with_module("planner"),
    now,
    allow_auto_push: cfg.defaults.allow_auto_push,
    disclosure: cfg.disclosure.clone(),
    agent_runtime: cfg.agent.vendor.to_string(),
    custom_instructions: cfg.clone(),
    agent_model: cfg.agent.model.clone(),
    agent_timeout: Duration::from_secs(cfg.agent.timeouts.planner_max_runtime_seconds),
    agent_idle_timeout: Duration::from_secs(cfg.agent.timeouts.planner_idle_timeout_seconds),
    discovery_policy: DiscoveryPolicy {
        auto_discovery: cfg.roles.planner.auto_discovery,
        labels: cfg.roles.planner.triggers.labels.clone(),
        label_mode: cfg.roles.planner.triggers.label_mode,
        require_assignee_current_user: cfg.roles.planner.triggers.require_assignee_current_user,
    },
    retry_base_delay: Duration::from_millis(cfg.scheduler.retry_base_delay_ms),
    retry_max_attempts: cfg.scheduler.retry_max_attempts,
    on_queue_item_enqueued: request_wake.clone(),
    on_agent_execution_started: notify_agent_execution_started.clone(),
})
```

**Reviewer, Fixer, Worker, Coordinator** — tuong tu voi options tuong ung moi role.

**Return:**
```rust
struct SchedulerHandlers {
    tick: Box<dyn Fn(Context) -> Result<()>>,
    claim: Box<dyn Fn(Context) -> Result<()>>,
    webhook: WebhookForwarder,
}
```

---

## 12. Error Boundary Mapping Per Runner Step

### 12.1 Failure Classification Logic (classifyFailureWithBoundary)

```rust
fn classify_failure(err: &Error, ctx: &FailureClassificationContext) -> QueueFailureKind {
    // 1. Neu la LoopError -> dung kind cua no
    if let Some(loop_err) = err.downcast_ref::<LoopError>() {
        return loop_err.kind();
    }

    // 2. Neu implements Temporary() -> RetryableTransient
    if let Some(temp) = err.downcast_ref::<dyn Temporary>() {
        if temp.temporary() {
            return QueueFailureKind::RetryableTransient;
        }
    }

    // 3. Neu context.Canceled / DeadlineExceeded -> RetryableTransient
    if err.is::<Cancelled>() || err.is::<TimeoutError>() {
        return QueueFailureKind::RetryableTransient;
    }

    // 4. Neu transient GitHub error -> RetryableTransient
    if is_github_transient_error(err) {
        return QueueFailureKind::RetryableTransient;
    }

    // 5. Fallback: boundary-based classification
    classify_by_boundary(err, ctx)
}

fn classify_by_boundary(err: &Error, ctx: &FailureClassificationContext) -> QueueFailureKind {
    // Determine boundary
    let boundary = match ctx.boundary {
        FailureBoundary::Unknown => extract_boundary(err),
        other => other,
    };

    let msg = err.to_string().to_lowercase();

    // Dirty worktree -> ManualIntervention
    if is_manual_worktree_message(&msg) || boundary == FailureBoundary::LocalWorktree {
        return QueueFailureKind::ManualIntervention;
    }

    // GQL unauthorized -> RetryableTransient
    if boundary == FailureBoundary::GitHubAPI && is_gql_unauthorized(&msg) {
        return QueueFailureKind::RetryableTransient;
    }

    // Deterministic denial -> NonRetryable
    if is_deterministic_denial(&msg) {
        return QueueFailureKind::NonRetryable;
    }

    // GitHub API 400/422 -> NonRetryable
    if boundary == FailureBoundary::GitHubAPI && is_http_4xx_denial(&msg) {
        return QueueFailureKind::NonRetryable;
    }

    // Internal deterministic boundaries -> NonRetryable
    if is_internal_deterministic_boundary(boundary) {
        return QueueFailureKind::NonRetryable;
    }

    // External boundaries -> RetryableTransient
    if is_external_boundary(boundary) {
        return QueueFailureKind::RetryableTransient;
    }

    // Fallback
    QueueFailureKind::NonRetryable
}

fn is_external_boundary(boundary: FailureBoundary) -> bool {
    matches!(boundary,
        FailureBoundary::GitRemote
        | FailureBoundary::GitHubAPI
        | FailureBoundary::ModelProvider
        | FailureBoundary::AgentProcess
    )
}

fn is_internal_deterministic_boundary(boundary: FailureBoundary) -> bool {
    matches!(boundary,
        FailureBoundary::GitLocal
        | FailureBoundary::Storage
        | FailureBoundary::Config
        | FailureBoundary::Checkpoint
        | FailureBoundary::Policy
    )
}
```

### 12.2 Complete Boundary Table

| Boundary | Classification | Ly do |
|----------|---------------|-------|
| `git_remote` | RetryableTransient | External — network, remote host |
| `git_local` | NonRetryable | Internal deterministic — conflict local |
| `github_api` | RetryableTransient | External — tru khi HTTP 400/422 thi NonRetryable |
| `model_provider` | RetryableTransient | External — LLM provider timeout/quota |
| `agent_process` | RetryableTransient | External — agent process crash |
| `local_worktree` | ManualIntervention | Worktree dirty, can human clean |
| `storage` | NonRetryable | Internal deterministic — DB error |
| `config` | NonRetryable | Internal — config sai |
| `checkpoint` | NonRetryable | Internal — checkpoint invariant violation |
| `policy` | NonRetryable | Internal — policy deny |
| `unknown` | NonRetryable | Fallback |

### 12.3 Planner Step Boundaries

| Step | Boundary |
|------|----------|
| `discover-issues` | `github_api` |
| `prepare-worktree` | `git_remote` |
| `write-spec` | `model_provider` |
| `publish` | `github_api` |
| `notify` | `github_api` |

### 12.4 Reviewer Step Boundaries

| Step | Boundary |
|------|----------|
| `review-pr` | `github_api` + `model_provider` |
| `check-worktree` | `git_local` |
| `submit-review` | `github_api` |
| `update-pr` | `github_api` |

### 12.5 Fixer Step Boundaries

| Step | Boundary |
|------|----------|
| `fix-pr` | `model_provider` + `github_api` |
| `prepare-worktree` | `git_remote` |
| `commit-fix` | `git_local` |
| `push-fix` | `git_remote` |

### 12.6 Worker Step Boundaries

| Step | Boundary |
|------|----------|
| `process-issue` | `github_api` + `model_provider` |
| `create-worktree` | `git_remote` |
| `implement` | `agent_process` |
| `commit-push` | `git_remote` |
| `open-pr` | `github_api` |

### 12.7 Retry Logic

```rust
struct RetryConfig {
    base_delay: Duration,     // ms (default: 1000)
    max_attempts: u32,        // default: 3 (planner/fixer/worker), 5 (reviewer)
}

fn should_retry_queue_item(item: &QueueItemRecord, kind: &QueueFailureKind) -> bool {
    match kind {
        QueueFailureKind::RetryableTransient => true,
        QueueFailureKind::RetryableAfterResume => true,
        QueueFailureKind::NonRetryable => item.max_attempts < 0 || item.attempts < item.max_attempts,
        QueueFailureKind::ManualIntervention => false,
    }
}

fn compute_retry_delay(attempt: u32) -> Duration {
    // Exponential backoff: baseDelay * 2^attempt, cap 300s
    let delay_ms = (1000 * 2u64.pow(attempt)).min(300_000);
    Duration::from_millis(delay_ms)
}
```

---

## 13. Deduplication & Locking

### 13.1 Dedupe Keys

| Item Type | Dedupe Key Format |
|-----------|------------------|
| Planner | `planner:{projectID}:{loopID}:{repo}:{issueNumber}` |
| Reviewer | `reviewer:{projectID}:{loopID}:{repo}:{prNumber}` |
| Fixer | `fixer:{loopID}` |
| Worker (issue) | `worker:{projectID}:{repo}:{issueNumber}` |
| Worker (PR) | `worker:{projectID}:{repo}:{prNumber}` |

### 13.2 Lock Keys

| Item Type | Lock Key Format |
|-----------|----------------|
| Planner | `issue:{repo}:{number}` |
| Reviewer | `pr:{repo}:{number}` |
| Fixer | `pr:{repo}:{number}` |
| Worker (issue) | `issue:{repo}:{number}` |
| Worker (PR) | `pr:{repo}:{number}` |

### 13.3 Critical: Fixer Blocked By Reviewer

Fixer items cho mot PR khong the claim khi co reviewer item (queued hoac running) cho cung PR do. Day la de tranh fixer sua code trong luc reviewer dang review.

```sql
-- Trong scheduledQueueBaseQuery:
AND (
    qi.type != 'fixer'
    OR qi.repo IS NULL
    OR qi.pr_number IS NULL
    OR NOT EXISTS (
        SELECT 1
        FROM queue_items blocker
        WHERE blocker.type = 'reviewer'
            AND blocker.repo = qi.repo
            AND blocker.pr_number = qi.pr_number
            AND blocker.status IN ('queued', 'running')
            AND blocker.id != qi.id
    )
)
```

---

## 14. Queue Item Dispatch Handler

Khi mot queue item duoc claim, no duoc dispatch den handler tuong ung:

| Type | Handler | Sync/Async |
|------|---------|------------|
| `"planner"` | `Planner.process_claimed_queue_item()` | Async (runner.Go) |
| `"reviewer"` | `Reviewer.process_claimed_queue_item()` | Async |
| `"fixer"` | `Fixer.process_claimed_queue_item()` | Async |
| `"worker"` | `Worker.process_claimed_queue_item()` | Async |
| `"snapshot"` | `process_snapshot_queue_item()` | Inline trong tick |

Snapshot handler xu ly inline (khong qua async runner) vi no nhanh va khong can agent.

---

## 15. Independent Claim Pump

Claim pump la vong lap rieng chay voi interval 1 giay. No chi claim — khong discovery.

```rust
async fn independent_claim_pass(ctx: Context, input: SchedulerTickInput) -> Result<()> {
    // Chi goi executeClaimPhase voi phase = "claim_pump"
    // Khong co discovery, khong co project iteration
    let discovered = HashSet::new();
    let _ = execute_claim_phase(ctx, Phase::ClaimPump, &discovered, false).await;
    Ok(())
}
```

Muc dich: dam bao items duoc claim nhanh nhat co the ma khong phai cho tick tiep theo (toi da 30s). Dac biet quan trong cho items duoc enqueue tu webhook.

---

## 16. Recovery Pipeline (Startup)

Khi scheduler khoi dong, recovery pipeline chay truoc:

### Phase 1: Orphan Agent Cleanup
1. `ListActive()` agent executions
2. For execution with PID > 0:
   - Verify process identity (`ps -p {pid} -o command=`)
   - If wrong process running → mark uncertain (ghi event, skip)
   - If correct process running → SIGTERM → 5s grace → SIGKILL
   - If not running → mark killed
3. Set `NativeResumeMode`

### Phase 2: Expired Lock Release
1. `ListExpired()` locks → release each → write event

### Phase 3: Stale Running Run Reconciliation
1. Get all "running" runs
2. Evaluate stale:
   - **Startup**: all runs
   - **Live**: only stale heartbeats (>30min)
3. If stale & not uncertain:
   - Mark run "interrupted"
   - Kill agent executions
   - Requeue or cancel queue items

### Phase 4: Loop State Normalization
1. Check terminal reviewer metadata → normalize loop status
2. If reviewer loop with `followUpdates` → attempt auto-recovery
3. If queue in `manual_intervention` → normalize
4. If running but no live agent → requeue
5. Handle stale queued loops (no active queue item)

### Phase 5: Reviewer Auto-Recovery (Deferred)
- Runs in background after startup
- For failed reviewer loops:
  - Skip if approval conditions met
  - Check retry policy → requeue with incremented attempts
  - Update loop metadata with `auto_recovery_attempts`

### Recovery Summary JSON
```json
{
  "startedAt": "2026-06-21T10:00:00.000Z",
  "completedAt": "2026-06-21T10:00:05.000Z",
  "orphanAgentCleanup": { "attempted": true, "cleanedCount": 5 },
  "expiredLocksReleased": 3,
  "interruptedRunsMarked": 2,
  "loopsRequeued": 1,
  "eventsWritten": 12
}
```

---

## 17. Process Identity Verification

```rust
async fn verify_process_identity(pid: u32, expected_command: &str) -> Result<(bool, bool)> {
    // Goi `ps -p {pid} -o command=` de lay actual command
    // So sanh filepath.Base cua first token
    // Tra ve (is_running, command_matches)
}
```

---

## 18. Logging & Observability

### 18.1 Tick Logging

Moi scheduler tick log:
- `durationMs`: thoi gian chay tick
- `claimedCount`: so item da claim
- `availableSlots`: so slot con trong
- `error`: neu co loi

### 18.2 Lane Logging

Moi discovery lane log:
- `lane`: ten lane (planner discovery, reviewer discovery, ...)
- `projectId`, `repo`
- `durationMs`
- `error`: neu co
- **Slow lane warning**: Neu lane chay > threshold (default 5s), log WARN

### 18.3 Claim Phase Logging

Moi claim phase log:
- `phase`: ten phase (pre_discovery, post_planner_discovery, ...)
- `availableSlots`
- `claimedCount`
- `durationMs`
- `error`: neu co

---

## 19. Queue Item Lifecycle

```
queued  ──[claim]──>  running  ──[complete]──>  completed
                         │
                    [fail/retry]
                         │
                         v
                      queued
                    (available_at updated)

running  ──[interrupt]──>  interrupted
running  ──[fail/non-retry]──>  failed
queued   ──[cancel]──>  cancelled
```

**States:**
- `queued`: San sang de claim (neu `available_at` <= now)
- `running`: Dang duoc xu ly
- `completed`: Da hoan thanh
- `failed`: That bai, khong retry
- `interrupted`: Bi kill (recovery, shutdown)
- `cancelled`: Bi huy bo

---


## 20. Key Design Decisions

1. **Zwei concurrent loops**: Main tick loop (30s interval + wake-driven) va claim pump (1s interval + wake-driven) de tranh discovery blocking claim.

2. **Two-bucket claim system**: Non-long-term retry items duoc uu tien tuyet doi. Long-term retry (>=5 attempts) chi claim khi con slot thua.

3. **Claim locking via Mutex**: `claim_mu` dam bao main tick va claim pump khong claim cung luc, tranh double-claim.

4. **Fixer blocked by reviewer**: fixer items khong the claim khi reviewer items cung PR con trong hoac dang chay.

5. **Lock key mechanism**: Prevent multiple items cung mot resource (issue, PR) claim dong thoi.

6. **Stale reconciliation on full slots**: Khi day slot, scheduler tu dong reconcile stale runs truoc khi claim lai.

7. **Fire-and-forget dispatch**: Items duoc dispatch qua `AsyncRunner.Go()` (tokio spawn) de khong block tick loop.

8. **Non-blocking wake channels**: `try_send` + capacity=1 de tranh blocking sender.
