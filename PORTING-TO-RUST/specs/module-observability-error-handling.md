# Module: Observability & Error Handling — Rust Port Spec

> Ngôn ngữ: Tiếng Việt (với thuật ngữ kỹ thuật tiếng Anh).
> Nguồn: `internal/bootstrap/logger.go`, `internal/eventlog/eventlog.go`, `internal/loops/failureclass/failureclass.go`, `internal/infra/github/errors.go`, `internal/infra/shell/runner.go`, `pkg/api/envelope.go`, `internal/api/handler.go`
> **Trung thực**: Mô tả chính xác Go state hiện tại, đề xuất cải tiến cho Rust port. Không tô hồng.

---

## 1. Observability Architecture

### 1.1 Current Go State

Go codebase hiện tại có 3 observability subsystems:

**Logger**
- Custom `Logger` interface (`internal/bootstrap/logger.go`) — KHÔNG dùng `log/slog`
- 4 methods: `Debug`, `Info`, `Warn`, `Error` — mỗi method nhận `(message string, context map[string]any)`
- Output format: JSON per line — `{"ts":"2026-06-21T10:00:00.000+07:00","level":"info","message":"...","context":{...}}`
- Timestamp format: local timezone (không phải UTC) — `2006-01-02T15:04:05.000-07:00`
- Log file: `looperd.log` trong `daemon.logDir` directory
- Log routing: Info → stdout; Warn/Error → stderr; Debug → file only (không stdout/stderr)
- Log rotation: by size (`MaxSizeMB`, default 10MB), retain `MaxFiles` archives (default 5)
- Thread-safe: `sync.Mutex` bao quanh write to file
- Log level priority: Debug=10, Info=20, Warn=30, Error=40 — entries below configured level bị drop
- KHÔNG có structured key-value pairs API — context là `map[string]any`
- KHÔNG có span/trace context propagation

**Event Log**
- `internal/eventlog/eventlog.go` — structured audit trail
- `AppendInput` struct với các field: `EventType`, `ProjectID`, `LoopID`, `RunID`, `EntityType/ID`, `CorrelationID`, `CausationID`, `ActorType/ID/DisplayName`, `Payload`/`PayloadJSON`
- Event ID format: `event_{16_hex_chars}` (crypto/rand), fallback `event_{unix_nano}`
- Timestamp: JavaScript ISO string — `2006-01-02T15:04:05.000Z` (UTC, 3-digit millisecond)
- Actor defaults: type="system", id="looperd", displayName="looperd"
- Stored in `event_logs` SQLite table (schema defined in module2-storage-sqlite.md)
- Indexed by `(entity_type, entity_id, created_at)` và `(event_type, created_at)`

**No Metrics Infrastructure**
- Go codebase KHÔNG có metrics (no prometheus, no expvar)
- KHÔNG có tracing (no opentelemetry, no jaeger)
- RequestID propagation chỉ qua API responses (field `requestId` trong envelope, không qua context propagation)
- Query parameter logging bị disabled (PII concerns)

### 1.2 Rust Port Design — Logging with tracing Crate

Thay thế custom Go Logger bằng `tracing` ecosystem (standard trong Rust async projects):

```toml
# Workspace dependencies (already declared in LOOPER_RUST_DESIGN.md)
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }
tracing-appender = "0.2"       # non-blocking file writer
```

#### Logger Architecture

```
Application Code
    │
    ▼
tracing macros: info!(field=value, "message")
trace! / debug! / info! / warn! / error!
    │
    ▼
tracing-subscriber
    ├── env-filter (RUST_LOG env var or config-based)
    ├── JSON layer → tracing-appender (non-blocking file)
    └── JSON/pretty layer → stdout/stderr
```

#### Initialization

```rust
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Registry};
use tracing_appender::non_blocking::NonBlocking;

struct ObservabilityConfig {
    log_dir: PathBuf,
    level: LogLevel,           // "debug"|"info"|"warn"|"error"
    max_size_mb: u32,          // 10 (file rotation)
    max_files: u32,            // 5
}

fn init_observability(cfg: &ObservabilityConfig) -> Result<WorkerGuard> {
    // 1. Ensure log directory exists
    std::fs::create_dir_all(&cfg.log_dir)?;

    // 2. File appender (non-blocking, rotated)
    let log_path = cfg.log_dir.join("looperd.log");
    let file_appender = tracing_appender::rolling::Builder::new()
        .max_size_bytes(cfg.max_size_mb as u64 * 1024 * 1024)
        .max_files(cfg.max_files)
        .build(log_path)?;
    let (non_blocking_file, _guard) = tracing_appender::non_blocking(file_appender);

    // 3. stdout/stderr routing
    let (stdout_writer, _stdout_guard) = tracing_appender::non_blocking(std::io::stdout());
    let (stderr_writer, _stderr_guard) = tracing_appender::non_blocking(std::io::stderr());

    // 4. Build subscriber with multiple layers
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(cfg.level.as_str()));

    let subscriber = Registry::default()
        .with(filter)
        .with(
            // File layer: JSON format, full structured data
            fmt::layer()
                .json()
                .with_writer(non_blocking_file)
                .with_target(true)
                .with_current_span(true)
                .with_span_list(true)
        )
        .with(
            // stdout: Info and above, JSON (machine-readable for logs-follow API)
            fmt::layer()
                .json()
                .with_writer(stdout_writer)
                .with_filter(LevelFilter::INFO)
        )
        .with(
            // stderr: Warn and above, JSON
            fmt::layer()
                .json()
                .with_writer(stderr_writer)
                .with_filter(LevelFilter::WARN)
        );

    tracing::subscriber::set_global_default(subscriber)
        .expect("global default subscriber already set");

    Ok(_guard)  // Drop guard flushes on shutdown
}
```

#### Log Level Enum

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

impl From<LogLevel> for tracing::Level {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Debug => tracing::Level::DEBUG,
            LogLevel::Info => tracing::Level::INFO,
            LogLevel::Warn => tracing::Level::WARN,
            LogLevel::Error => tracing::Level::ERROR,
        }
    }
}
```

#### Logging Conventions Per Layer

Mỗi layer trong system sử dụng structured fields nhất quán:

**Infra layer** (shell execution, git, github calls):
```rust
// Shell command
info!(
    target: "looper::infra::shell",
    command = %cmd,
    args = ?args,
    cwd = %cwd,
    duration_ms = elapsed.as_millis(),
    exit_code = result.exit_code,
    "shell command completed"
);

// On error
warn!(
    target: "looper::infra::shell",
    command = %cmd,
    exit_code = exit_code,
    stderr = %stderr_truncated,
    "shell command failed"
);
```

**Service layer** (loop, run, project services):
```rust
info!(
    target: "looper::service",
    loop_id = %loop_id,
    from_status = %from,
    to_status = %to,
    "loop status transition"
);
```

**Runner layer** (step execution):
```rust
info!(
    target: "looper::runner",
    runner = %runner_type,
    loop_id = %loop_id,
    run_id = %run_id,
    step = %step_name,
    "step started"
);
```

**API layer** (request/response):
```rust
info!(
    target: "looper::api",
    method = %method,
    path = %path,
    status = status,
    duration_ms = elapsed.as_millis(),
    request_id = %request_id,
    "api request handled"
);
```

#### PII-sensitive fields

Query parameters và request bodies không được log ở level Info trở xuống. Debug level có thể log sanitized request bodies nhưng phải có config gate:

```rust
fn sanitize_request_body(body: &str) -> String {
    // Redact sensitive fields: token, password, secret, key
    // Return truncated body (max 1KB)
}
```

Trong API middleware:
```rust
info!(
    target: "looper::api",
    method = %method,
    path = %path,       // path without query string
    status = status,
    duration_ms = elapsed,
    query = %sanitize_query(r.uri().query().unwrap_or("")),
    request_id = %request_id,
);
```

#### File Log Rotation

Log rotation trong Rust sử dụng `tracing-appender`'s rolling file appender:

| Config | Default | Behavior |
|--------|---------|----------|
| `logging.max_size_mb` | 10 | Rotate khi active file vượt quá N MB |
| `logging.max_files` | 5 | Giữ lại N-1 rotated files + active file |

Khác với Go custom implementation, `tracing-appender` có một số hạn chế:
- `max_size_bytes` và `max_files` chỉ available trong `rolling::Builder` từ `tracing-appender` >= 0.2
- Nếu không support rolling-by-size, cần wrapper tự implement (watch size, rename, reopen)
- Fallback: sử dụng `rolling::daily()` để rotation theo ngày (simpler, production-tested)

```rust
// Fallback: daily rotation (nếu size-based rotation không khả dụng)
fn init_file_appender(log_dir: &Path) -> (NonBlocking, WorkerGuard) {
    let file_appender = tracing_appender::rolling::daily(log_dir, "looperd.log");
    tracing_appender::non_blocking(file_appender)
}
```

### 1.3 Event Log System

Kế thừa design từ `module2-storage-sqlite.md` section 9.

#### Type-Safe Event Types

Chuyển từ string-based event types sang enum:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    // Looperd lifecycle
    LooperdStarted,
    LooperdShutdown,

    // Project lifecycle
    ProjectCreated,
    ProjectConfigUpdated,
    ProjectArchived,
    ProjectDeleted,

    // Loop lifecycle
    LoopCreated,
    LoopStatusChanged {
        from: LoopStatus,
        to: LoopStatus,
    },
    LoopDeleted,

    // Run lifecycle
    RunStarted,
    RunStepCompleted {
        step: String,
    },
    RunCompleted,
    RunFailed {
        failure_kind: FailureKind,
        failure_boundary: Option<FailureBoundary>,
    },

    // Agent lifecycle (from module5-agent-executor.md)
    AgentInvoked {
        vendor: AgentVendor,
    },
    AgentCompleted,
    AgentIdleTimeout,
    AgentMaxRuntimeTimeout,
    AgentKilled,
    AgentNativeResumeFallbackStarted,

    // Queue lifecycle
    QueueItemCreated {
        queue_type: String,
    },
    QueueItemClaimed,
    QueueItemCompleted,
    QueueItemFailed {
        failure_kind: FailureKind,
        attempts: u32,
    },
    QueueItemCancelled,

    // Runner steps
    RunnerStepStarted {
        runner: RunnerKind,
        step: String,
    },
    RunnerStepCompleted {
        runner: RunnerKind,
        step: String,
    },

    // Coordinator
    CoordinatorTriageCompleted,
    CoordinatorDispatch,
    CoordinatorMergeWatchResult,

    // Webhook
    WebhookReceived,
    WebhookTunnelOpened,
    WebhookTunnelClosed,

    // Recovery
    RecoveryPhaseCompleted {
        phase: String,
    },
    OrphanAgentCleanup,
    ExpiredLockReleased,
    StaleRunInterrupted,

    // Network
    NodeEnrolled,
    NodeHeartbeat,
    NodeLeaseAcquired,
    NodeLeaseLost,

    // Lock
    LockAcquired {
        key: String,
    },
    LockReleased {
        key: String,
    },
}
```

#### AppendInput (Event Log Service)

```rust
#[derive(Debug, Clone)]
pub struct AppendInput {
    pub id: Option<String>,
    pub event_type: EventType,
    pub project_id: Option<String>,
    pub loop_id: Option<String>,
    pub run_id: Option<String>,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub actor_type: Option<String>,
    pub actor_id: Option<String>,
    pub actor_display_name: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub created_at: Option<DateTime<Utc>>,
}

pub struct EventLogService {
    repos: Arc<Repositories>,
    now: fn() -> DateTime<Utc>,
}

impl EventLogService {
    pub fn append(&self, ctx: &Context, input: AppendInput) -> Result<EventLogRecord, StorageError> {
        // 1. Serialize payload
        let payload_json = input.payload
            .map(|p| serde_json::to_string(&p).unwrap_or_else(|_| "{}".to_string()))
            .unwrap_or_else(|| "{}".to_string());

        // 2. Generate ID if blank
        let id = input.id.unwrap_or_else(|| new_event_id("event"));

        // 3. Timestamp
        let created_at = input.created_at.unwrap_or_else(|| (self.now)());

        // 4. Actor defaults
        let actor_type = input.actor_type.unwrap_or_else(|| "system".to_string());
        let actor_id = input.actor_id.unwrap_or_else(|| "looperd".to_string());
        let actor_display_name = input.actor_display_name.unwrap_or_else(|| "looperd".to_string());

        // 5. Persist
        self.repos.events.append(ctx, EventLogRecord {
            id,
            event_type: serde_json::to_string(&input.event_type)?,
            project_id: input.project_id,
            loop_id: input.loop_id,
            run_id: input.run_id,
            entity_type: input.entity_type,
            entity_id: input.entity_id,
            correlation_id: input.correlation_id,
            causation_id: input.causation_id,
            actor_type: Some(actor_type),
            actor_id: Some(actor_id),
            actor_display_name: Some(actor_display_name),
            payload_json,
            created_at: format_javascript_iso_string(created_at),
        })
    }
}
```

#### Correlation ID Propagation

Correlation ID được sinh ra ở entry point (scheduler tick, webhook delivery, API request) và propagate xuyên suốt execution chain qua tracing span context:

```rust
fn generate_correlation_id() -> String {
    let mut raw = [0u8; 16];
    getrandom::getrandom(&mut raw).ok();
    format!("corr_{}", hex::encode(raw))
}

// Tại scheduler tick entry point:
let correlation_id = generate_correlation_id();
Span::current().record("correlation_id", &correlation_id);

// Mỗi downstream event dùng same correlation_id
event_log.append(&ctx, AppendInput {
    event_type: EventType::RunnerStepStarted { runner, step },
    correlation_id: Some(correlation_id.clone()),
    causation_id: Some(parent_event_id.clone()),
    // ...
});
```

#### Causation Chain

Mỗi event log entry có `causation_id` trỏ tới event gây ra nó. Ví dụ:

```
webhook.received (id: evt_a)  ← causation_id: null, correlation_id: corr_1
  └── queue_item.created (id: evt_b)  ← causation_id: evt_a, correlation_id: corr_1
       └── queue_item.claimed (id: evt_c)  ← causation_id: evt_b, correlation_id: corr_1
            └── run.started (id: evt_d)  ← causation_id: evt_c, correlation_id: corr_1
                 └── runner.step.started (id: evt_e)  ← causation_id: evt_d, correlation_id: corr_1
```

Chain này cho phép trace ngược từ failure event về root cause.

### 1.4 Metrics Design (New for Rust)

Go codebase KHÔNG có metrics infrastructure. Rust port bổ sung optional metrics subsystem, feature-gated:

```toml
[features]
default = []
metrics = ["dep:metrics", "dep:metrics-exporter-prometheus"]
```

#### What to Count (Counters)

| Metric Name | Labels | Trigger | Description |
|---|---|---|---|
| `looper_scheduler_ticks_total` | `project` | Per tick | Total scheduler ticks |
| `looper_queue_enqueue_total` | `type` (planner/reviewer/fixer/worker) | Enqueue | Queue items created |
| `looper_queue_claim_total` | `type` | Claim | Queue items claimed |
| `looper_queue_complete_total` | `type`, `status` (completed/failed/cancelled) | Completion | Queue items completed |
| `looper_agent_executions_total` | `vendor`, `status` | Execution end | Agent execution count |
| `looper_api_requests_total` | `method`, `path`, `status` | Request end | API request count |
| `looper_git_operations_total` | `operation` (clone/push/commit/fetch) | Op end | Git CLI operations |
| `looper_github_api_calls_total` | `endpoint` | Call end | GitHub API calls |
| `looper_events_appended_total` | `event_type` | Append | Event log entries |
| `looper_recovery_actions_total` | `phase` | Action | Recovery actions taken |

#### What to Histogram

| Metric Name | Labels | Buckets (ms) | Description |
|---|---|---|---|
| `looper_api_latency_seconds` | `method`, `path` | 0.01, 0.05, 0.1, 0.5, 1, 5, 10 | API response latency |
| `looper_agent_runtime_seconds` | `vendor`, `status` | 10, 30, 60, 120, 300, 600, 1800 | Agent execution duration |
| `looper_queue_wait_seconds` | `type` | 1, 5, 10, 30, 60, 300, 1800 | Time from enqueue to claim |
| `looper_github_command_seconds` | `command` (pr list, issue view, ...) | 0.5, 1, 2, 5, 10, 30, 60 | GitHub CLI command duration |
| `looper_runner_step_duration_seconds` | `runner`, `step` | 1, 5, 10, 30, 60, 300, 1800 | Runner step execution time |
| `looper_scheduler_tick_duration_seconds` | `project` | 0.1, 0.5, 1, 5, 10, 30 | Full tick duration |

#### What to Gauge

| Metric Name | Labels | Description |
|---|---|---|
| `looper_active_runs` | `runner` | Currently running runs |
| `looper_queue_depth` | `type`, `status` | Queue items by status |
| `looper_active_agents` | `vendor` | Currently running agent processes |
| `looper_project_count` | - | Registered project count |
| `looper_daemon_uptime_seconds` | - | Uptime since start |

#### Prometheus Export (Feature-Gated)

```rust
#[cfg(feature = "metrics")]
pub fn start_metrics_server(addr: SocketAddr) -> Result<()> {
    use metrics_exporter_prometheus::PrometheusBuilder;

    let builder = PrometheusBuilder::new();
    builder.listen_address(addr)?;
    builder.install()?;
    Ok(())
}

// Config addition:
pub struct MetricsConfig {
    pub enabled: bool,
    pub listen_addr: Option<SocketAddr>,  // defaults to 127.0.0.1:9100
}

#[cfg(not(feature = "metrics"))]
pub fn start_metrics_server(_: SocketAddr) -> Result<()> {
    tracing::info!("metrics disabled at compile time");
    Ok(())
}
```

#### Integration Pattern

Metrics records được instrument inline trong hot paths:

```rust
use metrics::{counter, histogram, gauge};

impl Scheduler {
    fn execute_tick(&self, project: &Project) {
        let start = Instant::now();
        // ... tick logic ...
        histogram!("looper_scheduler_tick_duration_seconds", start.elapsed(),
            "project" => project.id.clone());
        counter!("looper_scheduler_ticks_total", 1,
            "project" => project.id.clone());
    }
}
```

### 1.5 Tracing Design (New for Rust)

Go codebase KHÔNG có tracing. Rust port bổ sung distributed tracing spans sử dụng `tracing` crate native spans.

#### Span Hierarchy

```
scheduler_tick (project_id, correlation_id)
├── claim_phase
│   ├── claim_queue_item (queue_id, queue_type)
│   └── dispatch_runner (runner_type, loop_id)
├── runner_execution (loop_id, run_id, runner_type)
│   ├── step_execution (step_name)
│   │   ├── github_api_call (endpoint, repo)
│   │   ├── git_operation (operation, repo)
│   │   ├── agent_execution (vendor, model)
│   │   │   └── agent_process (pid, vendor)
│   │   └── event_log_append (event_type)
│   └── checkpoint_persist
└── coordinator_discovery (project_id)
    ├── triage_llm_call
    ├── dispatch_decision
    └── mergewatch_classify
```

#### Async Context Propagation in Tokio

`tracing` spans propagate automatically through async context khi sử dụng `Instrument`:

```rust
use tracing::Instrument;

impl Scheduler {
    async fn run(&self) {
        let span = tracing::info_span!(
            "scheduler_tick",
            project_id = %project.id,
            correlation_id = %corr_id
        );

        // Instrument attaches span to the future — spans follow async boundaries
        self.execute_tick(project, corr_id)
            .instrument(span)
            .await;
    }
}

// Inside a spawned task, instrumentation is critical:
tokio::spawn(
    runner.execute(step).instrument(
        tracing::info_span!(
            "runner_step",
            loop_id = %loop_id,
            step = %step.name(),
        )
    )
);
```

KHÔNG sử dụng `tokio::spawn` mà không có `.instrument()` — nếu thiếu Instrument, span context mất và tracing logs không có parent-child relationship.

#### Explicit Parent-Child Relationships

```rust
// Parent span tự động tạo child context khi instrument lồng nhau
#[tracing::instrument(skip(self), fields(loop_id, step))]
async fn execute_step(&self, loop_id: &str, step: &str) -> Result<()> {
    // Child span tự động inherit parent's span context
    let result = self.github.view_pull_request(input).await?;

    // Nếu cần manual span linkage (cross-task):
    let child_span = tracing::info_span!(
        parent: &Span::current(),  // explicit parent
        "agent_exec",
        vendor = %vendor
    );
    async { /* ... */ }.instrument(child_span).await;
}
```

#### Tracing Context on Outbound Calls

Khi gọi GitHub CLI (external process) hoặc agent subprocess, span context được inject qua environment variables:

```rust
fn inject_tracing_context() -> HashMap<String, String> {
    let mut env = HashMap::new();
    if let Some(ctx) = tracing::Span::current().context() {
        // Pass trace_id, span_id to child process
        env.insert("LOOPER_TRACE_ID".into(), ctx.trace_id().to_string());
        env.insert("LOOPER_SPAN_ID".into(), ctx.span_id().to_string());
    }
    env
}

// Agent process receives:
//   LOOPER_TRACE_ID=<hex>
//   LOOPER_SPAN_ID=<hex>
// (Không phải để propagte tracing vào agent, mà để log correlation trong agent output)
```

---

## 2. Error Handling Architecture

### 2.1 Current Go State

Go codebase có error classification system trải rộng qua nhiều packages:

**FailureKind** (4 values in `internal/loops/failureclass/failureclass.go`):
- `retryable_transient` — temporary errors (network, API timeout, 502/503)
- `retryable_after_resume` — can proceed after restart with resume context
- `non_retryable` — permanent failure, no retry
- `manual_intervention` — human required (dirty worktree, missing config)

**FailureBoundary** (11 values in `internal/loops/failureclass/failureclass.go`):
- `git_remote` — Git remote operations (clone, fetch, push)
- `git_local` — Git local operations (rebase, checkout, reset)
- `github_api` — GitHub API calls
- `model_provider` — LLM/agent provider errors
- `agent_process` — Agent subprocess errors
- `local_worktree` — Worktree state errors (dirty, manual intervention)
- `storage` — Database/storage errors
- `config` — Configuration errors
- `checkpoint` — Checkpoint serialization/state errors
- `policy` — Policy violations (branch protection, denied operations)
- `unknown` — Unclassified

**BoundaryError** wrapper: `WithBoundary(err, boundary)` attaches boundary to any error. Extracted via `errors.As`.

**Classification algorithm** (`Classify` in failureclass.go):
1. `context.Canceled` / `DeadlineExceeded` → `RetryableTransient`
2. `githubinfra.IsTransientError()` → `RetryableTransient`
3. Manual worktree messages ("dirty worktree", "uncommitted changes", "manual intervention required") → `ManualIntervention`
4. Boundary `GitHubAPI` + GraphQL 401 (not bad credentials) → `RetryableTransient`
5. Deterministic denial messages ("could not resolve to a pullrequest", "protected branch", "branch protection", "policy denied", "checkpoint invariant") → `NonRetryable`
6. GitHub API 400/422 → `NonRetryable`
7. Internal deterministic boundaries (GitLocal, Storage, Config, Checkpoint, Policy) → `NonRetryable`
8. External boundaries (GitRemote, GitHubAPI, ModelProvider, AgentProcess) → `RetryableTransient`
9. Default → `NonRetryable`

**Runner-level classification** (`classifyFailureWithBoundary`):
- Trước tiên check `*loopError` — nếu đã là loopError thì dùng thẳng kind của nó
- Kế đến check `context.Canceled` / `DeadlineExceeded` → RetryableTransient
- Kế đến check `githubinfra.IsTransientError()` → RetryableTransient
- Cuối cùng gọi `failureclass.Classify()` với boundary từ step mapping

**Step-to-Boundary mapping** (mỗi runner type có mapping riêng):

| Runner | Step | Boundary |
|--------|------|----------|
| Planner | discover-issues | github_api |
| Planner | prepare-worktree | git_remote |
| Planner | write-spec | model_provider |
| Planner | publish | github_api |
| Planner | notify | github_api |
| Reviewer | all steps | github_api (review/submit), model_provider (review agent) |
| Fixer | discover-pr/claim-pr | github_api |
| Fixer | prepare-worktree | git_remote |
| Fixer | repair/validate | model_provider/agent_process |
| Fixer | push/reconcile-commits | git_remote |
| Fixer | resolve-comments/recheck | github_api |
| Worker | prepare-work/open-pr | github_api |
| Worker | prepare-worktree | git_remote |
| Worker | plan/execute | model_provider |
| Worker | validate | agent_process |

**API Error Codes** (in `pkg/api/envelope.go`):

| ErrorCode | HTTP Status | Description |
|---|---|---|
| `ROUTE_NOT_FOUND` | 404 | Unknown API route |
| `METHOD_NOT_ALLOWED` | 405 | Wrong HTTP method |
| `UNAUTHORIZED` | 401 | Missing/invalid auth token |
| `AUTH_MISCONFIGURED` | 500 | Auth mode set but no token |
| `VALIDATION_FAILED` | 400 | Request validation error |
| `INTERNAL_ERROR` | 500 | Unexpected internal error |
| `PROJECT_NOT_FOUND` | 404 | Project not found |
| `LOOP_NOT_FOUND` | 404 | Loop not found |
| `RUN_NOT_FOUND` | 404 | Run not found |
| `PR_NOT_FOUND` | 404 | Pull request not found |
| `PULL_REQUEST_NOT_FOUND` | 404 | Pull request not found |
| `ACTIVE_RUN_NOT_FOUND` | 404 | Active run not found |
| `LOOP_CONFLICT` | 409 | Conflicting loop exists |
| `PROJECT_AMBIGUOUS` | 409 | Multiple projects match |
| `PROJECT_ID_CONFLICT` | 409 | Duplicate project ID |
| `PULL_REQUEST_PROJECT_MISMATCH` | 409 | PR belongs to different project |
| `AGENT_NOT_CONFIGURED` | 400 | Agent vendor not configured |
| `PROJECTS_UNAVAILABLE` | 500 | Projects service unavailable |
| `RUNTIME_CONTROL_UNAVAILABLE` | 501 | Runtime control endpoint not supported |

API error envelope format:
```json
{
  "ok": false,
  "error": {
    "code": "VALIDATION_FAILED",
    "message": "agent.vendor must be one of: claude-code, codex, opencode, cursor-cli, hermes",
    "details": null
  },
  "requestId": "req_abc123"
}
```

**GitHub TransientError** (in `internal/infra/github/errors.go`):
- `TransientError{Err}` wrapper — explicit transient classification
- `IsTransientError(err)` — checks both explicit TransientError AND pattern-based detection
- Pattern detection looks for known transient fragments:
  - TLS/network: "tls handshake timeout", "unexpected eof", "connection reset by peer", "connection refused/ timed out", "i/o timeout", "temporary failure in name resolution", "no such host", "network is unreachable"
  - HTTP/2: "stream error", "http2: server sent goaway"
  - HTTP status: "http 502/503/504", "502/503/504 *"
  - Rate limits: "secondary rate limit", "rate limit exceeded", "api rate limit exceeded"
  - GraphQL: "graphql: something went wrong"
- `ErrorMessage(err)` — extract best user-facing text from shell errors
- `IsPullRequestNotFoundError(err)` — "could not resolve to a pullrequest"
- `IsNotFoundError(err)` — "http 404" or " 404"
- `IsInaccessibleReviewRequestReviewerError(err)` — "resource not accessible" + "reviewrequests" + "requestedreviewer"

**Shell CommandExecutionError** (in `internal/infra/shell/runner.go`):
```go
type CommandExecutionError struct {
    Message string
    Result  Result  // { ExitCode, Stdout, Stderr, Duration }
}
```

**Storage errors**: SQLite errors wrapped through repository layer — no explicit error type enum, just string matching and sentinel errors (e.g., `ERR_QUEUE_ITEM_NOT_ACTIVE`).

### 2.2 Rust Port Design

#### Per-Crate Error Types

**looper-core** (error hub — shared types):

```rust
// === FAILURE CLASSIFICATION (from failureclass.go) ===

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    RetryableTransient,      // Temporary — retry with backoff
    RetryableAfterResume,    // Can proceed after restart/checkpoint resume
    NonRetryable,            // Permanent failure — no retry
    ManualIntervention,      // Human action required
}

impl FailureKind {
    pub fn should_retry(&self) -> bool {
        matches!(self, FailureKind::RetryableTransient | FailureKind::RetryableAfterResume)
    }

    pub fn requires_human(&self) -> bool {
        *self == FailureKind::ManualIntervention
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, FailureKind::NonRetryable | FailureKind::ManualIntervention)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureBoundary {
    GitRemote,
    GitLocal,
    GitHubApi,
    ModelProvider,
    AgentProcess,
    LocalWorktree,
    Storage,
    Config,
    Checkpoint,
    Policy,
    Unknown,
}

impl FailureBoundary {
    pub fn is_external(&self) -> bool {
        matches!(self,
            FailureBoundary::GitRemote |
            FailureBoundary::GitHubApi |
            FailureBoundary::ModelProvider |
            FailureBoundary::AgentProcess
        )
    }

    pub fn is_internal_deterministic(&self) -> bool {
        matches!(self,
            FailureBoundary::GitLocal |
            FailureBoundary::Storage |
            FailureBoundary::Config |
            FailureBoundary::Checkpoint |
            FailureBoundary::Policy
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureContext {
    pub runner: Option<RunnerKind>,
    pub step: Option<String>,
    pub boundary: FailureBoundary,
    pub side_effect_state: Option<String>,
}
```

**looper-core** (error types + classification):

```rust
// === CORE ERROR TYPES ===

/// Wrapper error that carries FailureBoundary information
/// Equivalent to Go's BoundaryError
#[derive(Debug)]
pub struct BoundError {
    pub boundary: FailureBoundary,
    pub source: Box<dyn std::error::Error + Send + Sync>,
}

impl std::fmt::Display for BoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.boundary, self.source)
    }
}

impl std::error::Error for BoundError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

pub fn with_boundary<E>(err: E, boundary: FailureBoundary) -> BoundError
where
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    BoundError {
        boundary,
        source: err.into(),
    }
}

// === CLASSIFICATION ENGINE ===

/// Classify an error into a FailureKind based on error type, boundary, and message content
pub fn classify_error(err: &(dyn std::error::Error + 'static), ctx: &FailureContext) -> FailureKind {
    // 1. Already classified errors (loop error with explicit kind)
    // Handled by each runner's classify_failure_with_boundary wrapper

    // 2. Context cancellation
    if err.downcast_ref::<tokio::time::error::Elapsed>().is_some() {
        return FailureKind::RetryableTransient;
    }
    if let Some(cause) = find_cause::<tokio::task::JoinError>(err) {
        if cause.is_cancelled() {
            return FailureKind::RetryableTransient;
        }
    }

    // 3. GitHub transient error detection
    if is_github_transient_error(err) {
        return FailureKind::RetryableTransient;
    }

    let message = err.to_string().to_lowercase();

    // 4. Manual worktree intervention
    if contains_any(&message, &[
        "dirty worktree", "worktree is dirty",
        "uncommitted changes", "manual intervention required"
    ]) || ctx.boundary == FailureBoundary::LocalWorktree {
        return FailureKind::ManualIntervention;
    }

    // 5. GitHub GraphQL 401 (not bad credentials) → retryable
    if ctx.boundary == FailureBoundary::GitHubApi
        && message.contains("graphql")
        && message.contains("401")
        && !contains_any(&message, &[
            "bad credentials", "authentication failed",
            "permission denied", "not authorized",
            "invalid token", "token expired",
        ])
    {
        return FailureKind::RetryableTransient;
    }

    // 6. Deterministic denials → NonRetryable
    if contains_any(&message, &[
        "could not resolve to a pullrequest",
        "could not resolve to an issue",
        "protected branch", "branch protection",
        "policy denied", "checkpoint invariant",
    ]) {
        return FailureKind::NonRetryable;
    }

    // 7. GitHub API 400/422 → NonRetryable (client error, not server error)
    if ctx.boundary == FailureBoundary::GitHubApi
        && contains_any(&message, &[
            "http 400", "http 422",
            "400 bad request", "422 unprocessable",
        ])
    {
        return FailureKind::NonRetryable;
    }

    // 8. Internal deterministic boundaries → NonRetryable
    if ctx.boundary.is_internal_deterministic() {
        return FailureKind::NonRetryable;
    }

    // 9. External boundaries → RetryableTransient
    if ctx.boundary.is_external() {
        return FailureKind::RetryableTransient;
    }

    // 10. Default
    FailureKind::NonRetryable
}

fn contains_any(message: &str, fragments: &[&str]) -> bool {
    fragments.iter().any(|f| message.contains(f))
}

fn find_cause<E: std::error::Error + 'static>(err: &(dyn std::error::Error + 'static)) -> Option<&E> {
    // Walk the error chain
    err.source()
        .and_then(|s| s.downcast_ref::<E>())
}
```

**looper-shell** (shell/infra layer):

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ShellError {
    #[error("command not found: {command}")]
    CommandNotFound { command: String },

    #[error("command timed out after {timeout}s: {command}")]
    Timeout {
        command: String,
        timeout: u64,
        stderr: String,
    },

    #[error("command exited with code {exit_code}: {command}")]
    NonZeroExit {
        command: String,
        exit_code: i32,
        stdout: String,        // truncated to 16KB
        stderr: String,        // truncated to 16KB
    },

    #[error("io error executing {command}: {source}")]
    IoError {
        command: String,
        source: std::io::Error,
    },

    #[error("internal shell error: {0}")]
    Internal(String),
}

impl ShellError {
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            ShellError::NonZeroExit { exit_code, .. } => Some(*exit_code),
            _ => None,
        }
    }

    pub fn stderr(&self) -> &str {
        match self {
            ShellError::NonZeroExit { stderr, .. } => stderr,
            ShellError::Timeout { stderr, .. } => stderr,
            _ => "",
        }
    }
}
```

**looper-github** (GitHub error types):

```rust
use thiserror::Error;
use std::time::Duration;

#[derive(Debug, Error)]
pub enum GhError {
    #[error("transient GitHub error: {message}")]
    Transient {
        message: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("not found: {resource}")]
    NotFound {
        resource: String,      // e.g., "PR #42 in owner/repo"
    },

    #[error("authentication failed: {detail}")]
    AuthFailed {
        detail: String,
    },

    #[error("rate limited: retry after {retry_after:?}")]
    RateLimited {
        retry_after: Duration,
    },

    #[error("parser error: expected {expected}, got {actual}")]
    ParseError {
        expected: &'static str,
        actual: String,
    },

    #[error("conflict: {detail}")]
    Conflict {
        detail: String,
    },

    #[error("GitHub CLI error ({exit_code}): {message}")]
    CliError {
        exit_code: i32,
        message: String,
        stderr: String,
    },

    #[error("internal gateway error: {0}")]
    Internal(String),
}

/// Pattern-based transient detection — ported from Go's transient message list
pub fn is_transient_gh_message(message: &str) -> bool {
    let message = message.to_lowercase();
    TRANSIENT_PATTERNS.iter().any(|p| message.contains(p))
}

const TRANSIENT_PATTERNS: &[&str] = &[
    // TLS / network
    "tls handshake timeout",
    "unexpected eof",
    "connection reset by peer",
    "connection refused",
    "connection timed out",
    "i/o timeout",
    "temporary failure in name resolution",
    "no such host",
    "network is unreachable",
    // HTTP/2
    "stream error",
    "http2: server sent goaway",
    // HTTP status
    "http 502", "502 bad gateway",
    "http 503", "503 service unavailable",
    "http 504", "504 gateway timeout",
    // Rate limits
    "secondary rate limit",
    "rate limit exceeded",
    "api rate limit exceeded",
    // GraphQL
    "graphql: something went wrong",
];

pub fn is_github_transient_error(err: &(dyn std::error::Error + 'static)) -> bool {
    // Check for explicit GhError::Transient
    if let Some(gh_err) = err.downcast_ref::<GhError>() {
        return matches!(gh_err, GhError::Transient { .. });
    }

    // Pattern-based detection for shell error messages
    let message = error_message(err);
    if !looks_like_github_failure(&message) {
        return false;
    }
    is_transient_gh_message(&message)
}

fn looks_like_github_failure(message: &str) -> bool {
    let m = message.to_lowercase();
    GITHUB_FINGERPRINTS.iter().any(|f| m.contains(f))
}

const GITHUB_FINGERPRINTS: &[&str] = &[
    "github",
    "api.github.com",
    "graphql",
    "gh api",
    "gh pr",
];

pub fn error_message(err: &(dyn std::error::Error + 'static)) -> String {
    // Walk error chain, combine stderr from ShellError if available
    // similar to Go's ErrorMessage() which extracts shell output
    // ...
}

pub fn is_not_found_error(err: &(dyn std::error::Error + 'static)) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("http 404") || msg.contains(" 404") || msg.contains("not found")
}

pub fn is_pull_request_not_found_error(err: &(dyn std::error::Error + 'static)) -> bool {
    err.to_string().to_lowercase().contains("could not resolve to a pullrequest")
}
```

**looper-storage** (storage errors):

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("not found: {entity}")]
    NotFound { entity: String },

    #[error("duplicate entry: {detail}")]
    Duplicate { detail: String },

    #[error("constraint violation: {detail}")]
    ConstraintViolation { detail: String },

    #[error("migration error: {detail}")]
    MigrationError { detail: String },

    #[error("connection error: {source}")]
    ConnectionError {
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("transaction error: {source}")]
    TransactionError {
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("queue item not active")]
    QueueItemNotActive,

    #[error("internal storage error: {0}")]
    Internal(String),
}
```

**looper-service** (service/domain layer):

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("not found: {entity_type} with id {entity_id}")]
    NotFound {
        entity_type: &'static str,
        entity_id: String,
    },

    #[error("conflict: {detail}")]
    Conflict { detail: String },

    #[error("invalid transition: {from} -> {to}")]
    InvalidTransition {
        from: String,
        to: String,
    },

    #[error("validation failed: {detail}")]
    ValidationFailed { detail: String },

    #[error("internal error: {detail}")]
    Internal { detail: String },
}
```

**looper-runner** (runner errors):

```rust
use thiserror::Error;
use crate::core::{FailureKind, FailureBoundary};

#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("step '{step}' failed: {kind:?}")]
    StepFailed {
        step: String,
        kind: FailureKind,
        boundary: FailureBoundary,
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("checkpoint error: {detail}")]
    CheckpointError { detail: String },

    #[error("agent execution error: {detail}")]
    AgentError { detail: String },

    #[error("step precondition failed: {detail}")]
    PreconditionFailed { detail: String },

    #[error("resume error: {detail}")]
    ResumeError { detail: String },

    #[error("internal runner error: {0}")]
    Internal(String),
}

impl RunnerError {
    pub fn failure_kind(&self) -> Option<FailureKind> {
        match self {
            RunnerError::StepFailed { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    pub fn failure_boundary(&self) -> Option<FailureBoundary> {
        match self {
            RunnerError::StepFailed { boundary, .. } => Some(*boundary),
            _ => None,
        }
    }
}
```

**looper-api** (API errors):

```rust
use thiserror::Error;
use http::StatusCode;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("route not found: {path}")]
    RouteNotFound { path: String },

    #[error("method not allowed for {path}")]
    MethodNotAllowed { path: String, method: String },

    #[error("unauthorized")]
    Unauthorized,

    #[error("auth misconfigured: {detail}")]
    AuthMisconfigured { detail: String },

    #[error("validation failed: {detail}")]
    ValidationFailed { detail: String, details: Option<serde_json::Value> },

    #[error("not found: {resource}")]
    NotFound { resource: String },

    #[error("conflict: {detail}")]
    Conflict { detail: String },

    #[error("internal error")]
    Internal {
        message: String,       // logged but never sent to client
    },
}

impl ApiError {
    pub fn error_code(&self) -> &'static str {
        match self {
            ApiError::RouteNotFound { .. } => "ROUTE_NOT_FOUND",
            ApiError::MethodNotAllowed { .. } => "METHOD_NOT_ALLOWED",
            ApiError::Unauthorized => "UNAUTHORIZED",
            ApiError::AuthMisconfigured { .. } => "AUTH_MISCONFIGURED",
            ApiError::ValidationFailed { .. } => "VALIDATION_FAILED",
            ApiError::NotFound { .. } => "NOT_FOUND",
            ApiError::Conflict { .. } => "CONFLICT",
            ApiError::Internal { .. } => "INTERNAL_ERROR",
        }
    }

    pub fn http_status(&self) -> StatusCode {
        match self {
            ApiError::RouteNotFound { .. } => StatusCode::NOT_FOUND,
            ApiError::MethodNotAllowed { .. } => StatusCode::METHOD_NOT_ALLOWED,
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::AuthMisconfigured { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::ValidationFailed { .. } => StatusCode::BAD_REQUEST,
            ApiError::NotFound { .. } => StatusCode::NOT_FOUND,
            ApiError::Conflict { .. } => StatusCode::CONFLICT,
            ApiError::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn sanitize(self) -> Self {
        // Never expose internal error details to unauthorized clients
        match self {
            ApiError::Internal { .. } => ApiError::Internal {
                message: "an internal error occurred".to_string(),
            },
            other => other,
        }
    }
}
```

**looper-config** (config validation errors):

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("validation failed with {count} issue(s)")]
    ValidationError {
        issues: Vec<ValidationIssue>,
    },

    #[error("file not found: {path}")]
    FileNotFound { path: String },

    #[error("parse error in {path}: {detail}")]
    ParseError { path: String, detail: String },

    #[error("io error reading {path}: {source}")]
    IoError {
        path: String,
        source: std::io::Error,
    },

    #[error("internal config error: {0}")]
    Internal(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub path: String,          // e.g., "server.port"
    pub message: String,       // e.g., "must be between 1 and 65535"
    pub severity: IssueSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Error,
    Warning,
}

impl ConfigError {
    pub fn into_api_error(self) -> ApiError {
        match self {
            ConfigError::ValidationError { issues } => {
                let details = serde_json::to_value(
                    issues.iter().map(|i| {
                        serde_json::json!({"path": i.path, "message": i.message})
                    }).collect::<Vec<_>>()
                ).ok();
                ApiError::ValidationFailed {
                    detail: format!("{} validation issue(s)", issues.len()),
                    details,
                }
            }
            other => ApiError::Internal {
                message: other.to_string(),
            }
        }
    }
}
```

### 2.3 Error Propagation Rules

#### Cross-Crate Error Conversion (From impls)

```rust
// looper-shell → looper-github
impl From<ShellError> for GhError {
    fn from(err: ShellError) -> Self {
        match err {
            ShellError::NonZeroExit { command, exit_code, stdout, stderr } => {
                let message = format!("{}: {}", command, stderr);
                if is_transient_gh_message(&message) {
                    GhError::Transient { message, source: Some(err.into()) }
                } else if err.to_string().contains("404") {
                    GhError::NotFound { resource: command }
                } else {
                    GhError::CliError { exit_code, message, stderr }
                }
            }
            ShellError::Timeout { command, .. } => {
                GhError::Transient {
                    message: format!("{} timed out", command),
                    source: Some(err.into()),
                }
            }
            ShellError::CommandNotFound { command } => {
                GhError::Internal(format!("command not found: {}", command))
            }
            ShellError::IoError { command, .. } => {
                GhError::Transient {
                    message: format!("io error for {}", command),
                    source: Some(err.into()),
                }
            }
            ShellError::Internal(msg) => GhError::Internal(msg),
        }
    }
}

// looper-storage → looper-service
impl From<StorageError> for ServiceError {
    fn from(err: StorageError) -> Self {
        match err {
            StorageError::NotFound { entity } => ServiceError::NotFound {
                entity_type: "storage entity",
                entity_id: entity,
            },
            StorageError::Duplicate { detail } => ServiceError::Conflict {
                detail: format!("duplicate: {}", detail),
            },
            StorageError::ConstraintViolation { detail } => ServiceError::ValidationFailed {
                detail: format!("constraint: {}", detail),
            },
            StorageError::QueueItemNotActive => ServiceError::Conflict {
                detail: "queue item not active".to_string(),
            },
            _ => ServiceError::Internal {
                detail: err.to_string(),
            },
        }
    }
}

// looper-github → looper-runner
impl From<(GhError, String, FailureBoundary)> for RunnerError {
    fn from((err, step, boundary): (GhError, String, FailureBoundary)) -> Self {
        let kind = if is_github_transient_error(&err) {
            FailureKind::RetryableTransient
        } else {
            match &err {
                GhError::NotFound { .. } | GhError::Conflict { .. } => FailureKind::NonRetryable,
                GhError::AuthFailed { .. } => FailureKind::ManualIntervention,
                GhError::RateLimited { .. } => FailureKind::RetryableTransient,
                GhError::ParseError { .. } => FailureKind::NonRetryable,
                _ => FailureKind::RetryableTransient,
            }
        };

        RunnerError::StepFailed {
            step,
            kind,
            boundary,
            source: Box::new(err),
        }
    }
}

// looper-service → looper-api
impl From<ServiceError> for ApiError {
    fn from(err: ServiceError) -> Self {
        match err {
            ServiceError::NotFound { entity_type, entity_id } => ApiError::NotFound {
                resource: format!("{}:{}", entity_type, entity_id),
            },
            ServiceError::Conflict { detail } => ApiError::Conflict { detail },
            ServiceError::InvalidTransition { from, to } => ApiError::ValidationFailed {
                detail: format!("invalid transition from {} to {}", from, to),
                details: None,
            },
            ServiceError::ValidationFailed { detail } => ApiError::ValidationFailed {
                detail,
                details: None,
            },
            ServiceError::Internal { detail } => ApiError::Internal { message: detail },
        }
    }
}

// looper-runner → looper-scheduler (queue)
impl From<RunnerError> for QueueFailure {
    fn from(err: RunnerError) -> Self {
        QueueFailure {
            kind: err.failure_kind().unwrap_or(FailureKind::NonRetryable),
            message: err.to_string(),
        }
    }
}
```

#### Propagation Policy by Layer

```
LAYER            CAN RECEIVE                    CAN PROPAGATE TO                 SANITIZED?
─────            ────────────                    ────────────────                 ──────────
Shell            -                               GhError                          N/A (infra)
GitHub           ShellError                      GhError → RunnerError            No
Git              ShellError                      RunnerError                      No
Storage          rusqlite::Error                 StorageError → ServiceError     Yes (internal details stripped)
Service          StorageError, GhError            ServiceError                    Yes (PII and internal state stripped)
Runner           ServiceError, GhError,           RunnerError → QueueFailure      Partial (summary only)
                 ShellError, AgentError
Scheduler        RunnerError                     QueueFailure (DB)               Partial
API              ServiceError, RunnerError        ApiError (HTTP response)        Yes (full sanitization)
CLI              ServiceError, ApiError           Human-readable output           Yes (user-facing)
```

#### Policy: Never Leak Internal Errors

**API Layer**: Before returning any error to HTTP client, apply `sanitize()`:
- `ApiError::Internal { message }` → `ApiError::Internal { message: "an internal error occurred" }`
- Never include `file:line`, stack traces, SQL queries, or environment details
- Internal error full text is logged via `tracing::error!` with structured fields

```rust
impl Handler {
    fn handle_error(&self, err: impl Into<ApiError>) -> Response {
        let api_err = err.into().sanitize();

        // Always log the full error
        tracing::error!(
            target: "looper::api",
            error_code = %api_err.error_code(),
            error = %api_err,           // Display impl shows sanitized message
            ?err,                        // Debug of original (logged but hidden in sanitized)
            "api error response"
        );

        // Build JSON response with sanitized error
        let body = serde_json::json!({
            "ok": false,
            "error": {
                "code": api_err.error_code(),
                "message": api_err.to_string(),
            },
            "requestId": current_request_id(),
        });

        (api_err.http_status(), Json(body))
    }
}
```

**CLI Layer**: Error display có hai modes:

```rust
pub fn format_error(err: &dyn std::error::Error, verbose: bool) -> String {
    if verbose {
        // Full error chain with sources
        format_error_chain(err)
    } else {
        // First error message only, user-friendly
        err.to_string()
    }
}
```

Verbose mode được kích hoạt bởi:
- `--verbose` / `-v` flag trên CLI invocation
- `LOOPER_VERBOSE=1` environment variable
- Mặc định là non-verbose (user-friendly)

#### Error Display: User-Friendly vs Debug-Verbose

| Context | Display Mode | Example |
|---------|-------------|---------|
| CLI output (default) | User-friendly | `Error: failed to create worktree for issue #42` |
| CLI output (verbose) | Full chain | `Error: failed to create worktree for issue #42\nCaused by: git worktree add failed (exit code 128)\n  stderr: fatal: not a git repository` |
| API response | Sanitized code+message | `{"code":"INTERNAL_ERROR","message":"an internal error occurred"}` |
| API log | Full detail | `error="storage connection failed: ..." error_code=INTERNAL_ERROR path=/api/v1/loops` |
| Event log entry | Summary | `run.failed { failure_kind: "retryable_transient", failure_boundary: "github_api" }` |
| Queue last_error | Truncated | Truncated to 512 bytes, no stack trace |

#### Sanitization Rules

| Original Detail | Sanitized For |
|----------------|---------------|
| SQL query text | API/CLI only (logged internally) |
| File paths (absolute) | Partial — `/home/user/.looper/...` → `~/.looper/...` (CLI), stripped (API) |
| Auth tokens / secrets | `[REDACTED]` |
| API keys in error messages | `[REDACTED]` |
| Stack traces | Stripped from user-facing, logged internally |
| DB connection strings | `postgresql://user:***@host/db` |
| GitHub personal tokens | `ghp_****` (first 4 chars shown for identification) |
| Internal hostnames/IPs | Stripped from API, shown in CLI verbose |
| Agent model provider keys | `[REDACTED]` |

---

## 3. Logged Events Catalog (Complete)

### 3.1 Looperd Lifecycle

| Event Type | Payload Fields | Emitted By |
|---|---|---|
| `looperd.started` | `{version, pid, os, arch, config_path}` | bootstrap |
| `looperd.shutdown` | `{reason, uptime_seconds}` | runtime.Stop |
| `recovery.phase_completed` | `{phase, details}` | recovery pipeline |
| `orphan_agent.cleaned` | `{execution_id, pid, signal}` | recovery phase 1 |
| `expired_lock.released` | `{lock_key, owner}` | recovery phase 2 |
| `stale_run.interrupted` | `{run_id, loop_id}` | recovery phase 3 |

### 3.2 Project Lifecycle

| Event Type | Payload Fields | Emitted By |
|---|---|---|
| `project.created` | `{name, repo_path}` | ProjectsService.Create |
| `project.config_updated` | `{changed_fields}` | ProjectsService.UpdateConfig |
| `project.archived` | `{}` | ProjectsService.Archive |
| `project.deleted` | `{}` | ProjectsService.Delete |

### 3.3 Loop Lifecycle

| Event Type | Payload Fields | Emitted By |
|---|---|---|
| `loop.created` | `{type, target_type, target_id, seq}` | LoopsService.Create |
| `loop.status_changed` | `{from, to}` | LoopsService.TransitionStatus |
| `loop.deleted` | `{type}` | Scheduler/cleanup |
| `loop.terminated` | `{reason}` | Scheduler/StopLoop |

### 3.4 Run Lifecycle

| Event Type | Payload Fields | Emitted By |
|---|---|---|
| `run.started` | `{loop_id, loop_seq, start_step}` | Runner |
| `run.step_completed` | `{step, execution_time_ms}` | Runner |
| `run.completed` | `{loop_id, summary}` | Runner |
| `run.failed` | `{step, kind, boundary, summary}` | Runner.classifyFailure |

### 3.5 Queue Lifecycle

| Event Type | Payload Fields | Emitted By |
|---|---|---|
| `queue.item_created` | `{queue_type, dedupe_key, priority}` | Scheduler enqueue |
| `queue.item_claimed` | `{item_id, claimed_by}` | Scheduler claim |
| `queue.item_completed` | `{item_id, type}` | Runner completion |
| `queue.item_failed` | `{item_id, kind, attempts}` | Runner failure handler |
| `queue.item_cancelled` | `{item_id, reason}` | Scheduler/StopLoop |
| `queue.item_requeued` | `{item_id, reason, attempts}` | Recovery/requeue |

### 3.6 Agent Execution

| Event Type | Payload Fields | Emitted By |
|---|---|---|
| `agent.invoked` | `{vendor, model, pid, execution_id}` | Agent executor |
| `agent.completed` | `{summary, parse_status, duration_s}` | Agent executor |
| `agent.idle_timeout` | `{idle_timeout_s, last_progress_at}` | Agent executor |
| `agent.max_runtime_timeout` | `{max_runtime_s}` | Agent executor |
| `agent.killed` | `{reason}` | Agent executor |
| `agent.native_resume_started` | `{session_id, mode}` | Agent executor |
| `agent.native_resume_fallback` | `{reason, original_session_id}` | Agent executor |

### 3.7 Coordinator

| Event Type | Payload Fields | Emitted By |
|---|---|---|
| `coordinator.triage_completed` | `{issue_number, disposition}` | Coordinator |
| `coordinator.dispatch` | `{issue_number, to_runner}` | Coordinator dispatch |
| `coordinator.mergewatch_result` | `{pr_number, action}` | Merge watch classifier |

### 3.8 Webhook

| Event Type | Payload Fields | Emitted By |
|---|---|---|
| `webhook.received` | `{delivery_id, event, action}` | Webhook forwarder |
| `webhook.tunnel_opened` | `{repo, hook_id, public_url}` | Webhook tunnel |
| `webhook.tunnel_closed` | `{repo, hook_id, reason}` | Webhook tunnel |
| `webhook.forwarded` | `{project_id, lanes}` | Webhook forwarder execution |

---

## 4. Testing Strategy for Observability

### 4.1 Logger Testing

```rust
#[cfg(test)]
mod tests {
    use tracing_subscriber::fmt::TestWriter;
    use tracing_subscriber::prelude::*;

    #[test]
    fn test_log_format() {
        // Use TestWriter to capture log output
        let subscriber = tracing_subscriber::fmt()
            .with_writer(TestWriter::new())
            .with_test_writer()  // for test output capture
            .finish();

        // See existing Go test patterns in
        // internal/bootstrap/logger_test.go for reference
    }

    #[test]
    fn test_log_level_filtering() {
        // Verify debug messages are filtered when level=info
    }

    #[test]
    fn test_log_rotation_by_size() {
        // Write enough to trigger rotation, verify archive exists
        // Reference: TestCreateLoggerRotatesBySizeAndRetainsMaxFiles
    }

    #[test]
    fn test_log_rotation_retention() {
        // Verify MaxFiles retention
        // Reference: TestCreateLoggerRotatesArchiveChainAndCleansStaleFiles
    }
}
```

### 4.2 Event Log Testing

```rust
#[cfg(test)]
mod tests {
    // Test patterns:
    // - Event ID generation format
    // - Actor defaulting
    // - Payload serialization
    // - Causation chain integrity
    // - Correlation ID propagation through span context
    // - Event type enum serialization/deserialization
}
```

### 4.3 Metrics Testing (Feature-Gated)

```rust
#[cfg(all(test, feature = "metrics"))]
mod tests {
    #[test]
    fn test_counter_increments() {
        // Verify counter value post-increment
    }

    #[test]
    fn test_histogram_records() {
        // Verify histogram observation
    }

    #[test]
    fn test_prometheus_endpoint() {
        // Start metrics server, scrape /metrics, verify labels
    }
}
```

### 4.4 Error Classification Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_transient_boundary() {
        // External boundaries → RetryableTransient
        assert_eq!(
            classify_error(&MockError::new("network error"),
                &FailureContext { boundary: FailureBoundary::GitHubApi, ..default() }),
            FailureKind::RetryableTransient
        );
    }

    #[test]
    fn test_classify_non_retryable_boundary() {
        // Internal deterministic boundaries → NonRetryable
        assert_eq!(
            classify_error(&MockError::new("storage error"),
                &FailureContext { boundary: FailureBoundary::Storage, ..default() }),
            FailureKind::NonRetryable
        );
    }

    #[test]
    fn test_classify_deterministic_denial() {
        // "protected branch" → NonRetryable
        assert_eq!(
            classify_error(&MockError::new("protected branch violation"),
                &FailureContext { boundary: FailureBoundary::GitHubApi, ..default() }),
            FailureKind::NonRetryable
        );
    }

    #[test]
    fn test_classify_manual_worktree() {
        // "dirty worktree" → ManualIntervention
        assert_eq!(
            classify_error(&MockError::new("worktree is dirty"),
                &FailureContext { boundary: FailureBoundary::LocalWorktree, ..default() }),
            FailureKind::ManualIntervention
        );
    }

    #[test]
    fn test_classify_github_400_non_retryable() {
        assert_eq!(
            classify_error(&MockError::new("HTTP 422 Unprocessable Entity"),
                &FailureContext { boundary: FailureBoundary::GitHubApi, ..default() }),
            FailureKind::NonRetryable
        );
    }

    #[test]
    fn test_classify_github_transient_message() {
        assert!(is_transient_gh_message("connection reset by peer"));
        assert!(is_transient_gh_message("HTTP 502 Bad Gateway"));
        assert!(is_transient_gh_message("api rate limit exceeded"));
        assert!(!is_transient_gh_message("HTTP 404 Not Found"));
    }

    #[test]
    fn test_classify_context_cancelled() {
        // DeadlineExceeded → RetryableTransient
    }

    #[test]
    fn test_github_transient_error_patterns() {
        // Verify all transient patterns from Go
        let patterns = [
            "tls handshake timeout", "unexpected eof",
            "connection reset by peer", "connection refused",
            "connection timed out", "i/o timeout",
            "temporary failure in name resolution", "no such host",
            "network is unreachable", "stream error",
            "http2: server sent goaway",
            "http 502", "502 bad gateway",
            "http 503", "503 service unavailable",
            "http 504", "504 gateway timeout",
            "secondary rate limit", "rate limit exceeded",
            "api rate limit exceeded",
            "graphql: something went wrong",
        ];
        for pattern in &patterns {
            assert!(is_transient_gh_message(pattern),
                "pattern should be transient: {}", pattern);
        }
    }
}
```

### 4.5 API Error Sanitization Testing

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_internal_error_sanitized() {
        let api_err = ApiError::Internal {
            message: "connection to SQLite at /home/user/.looper/db.sqlite failed: permission denied".to_string(),
        };
        let sanitized = api_err.sanitize();
        assert_eq!(sanitized.to_string(), "an internal error occurred");
    }

    #[test]
    fn test_validation_error_not_sanitized() {
        let api_err = ApiError::ValidationFailed {
            detail: "server.port must be between 1 and 65535".to_string(),
            details: None,
        };
        let sanitized = api_err.sanitize();
        assert_eq!(sanitized.to_string(), "server.port must be between 1 and 65535");
    }

    #[test]
    fn test_api_error_http_status_mapping() {
        assert_eq!(ApiError::Unauthorized.http_status(), StatusCode::UNAUTHORIZED);
        assert_eq!(ApiError::RouteNotFound { path: "/x".into() }.http_status(), StatusCode::NOT_FOUND);
        assert_eq!(ApiError::Internal { message: "x".into() }.http_status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(ApiError::ValidationFailed { detail: "x".into(), details: None }.http_status(), StatusCode::BAD_REQUEST);
    }
}
```
