# Module 8: API Server + Webhook Forwarder — Rust Spec

> Derived from `internal/api/handler.go` (5540 lines), `internal/api/server.go` (92 lines), `internal/api/reviewer_repair.go`, `internal/webhookforward/forwarder.go` (753 lines), `pkg/api/envelope.go` (128 lines)
>
> Base path: `/api/v1`

---

## 1. Type Definitions (Kieu du lieu)

### 1.1 ErrorCode Enum

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    ActiveRunNotFound,          // 404
    AgentNotConfigured,         // 400
    AuthMisconfigured,          // 500
    InternalError,              // 500
    LoopConflict,               // 409
    LoopNotFound,               // 404
    MethodNotAllowed,           // 405
    ProjectsUnavailable,        // 500
    ProjectAmbiguous,           // 409
    ProjectIdConflict,          // 409
    ProjectNotFound,            // 404
    PrNotFound,                 // 404
    PullRequestNotFound,        // 404
    PullRequestProjectMismatch, // 409
    RouteNotFound,              // 404
    RunNotFound,                // 404
    RuntimeControlUnavailable,  // 501
    Unauthorized,               // 401
    ValidationFailed,           // 400
}
```

**HTTP status mapping**: `ErrorCode::Status()` method tra ve status code nhu tren. Default fallback: 500.

### 1.2 ErrorInfo (Error struct trong Go)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorInfo {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}
```

### 1.3 Envelope (generic response wrapper)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
    pub request_id: String,
}
```

**Constructor functions**:
- `Envelope::success(request_id: String, data: T) -> Envelope<T>` — sets `ok: true`, `data: Some(data)`, `error: None`.
- `Envelope::failure(request_id: String, code: ErrorCode, message: String, details: Option<Value>) -> Envelope<Value>` — sets `ok: false`, `data: None`, `error: Some(...)`.

### 1.4 RequestID Generation

- Format: UUID-style hex string, prefix `req_` KHONG dung nua (Go dung hex UUID4 khong prefix).
- Go implementation: 16 bytes random, set version/variant bits, format nhu UUID: `xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx`.
- Fallback khi `rand.Read` fail: `req-{unix_nanos}`.
- Client cung co the set header `X-Request-ID` de override.

### 1.5 ApiError (internal error type)

```rust
#[derive(Debug)]
pub struct ApiError {
    pub code: ErrorCode,
    pub status: StatusCode,
    pub message: String,
    pub details: Option<serde_json::Value>,
}
```

**Helper constructors**:
- `ApiError::internal_server_error(err: impl Into<String>) -> ApiError` — code INTERNAL_ERROR, status 500.
- `ApiError::method_not_allowed(path: &str) -> ApiError` — code METHOD_NOT_ALLOWED, status 405.
- `ApiError::route_not_found(path: &str) -> ApiError` — code ROUTE_NOT_FOUND, status 404.
- `ApiError::validation_failed(msg: impl Into<String>) -> ApiError` — code VALIDATION_FAILED, status 400.
- `ApiError::unauthorized(msg: impl Into<String>) -> ApiError` — code UNAUTHORIZED, status 401.
- `ApiError::auth_misconfigured(msg: impl Into<String>) -> ApiError` — code AUTH_MISCONFIGURED, status 500.
- `ApiError::loop_not_found(selector: &str) -> ApiError` — code LOOP_NOT_FOUND, status 404.
- `ApiError::loop_conflict(project_id, loop_type, target_key) -> ApiError` — code LOOP_CONFLICT, status 409.
- `ApiError::project_not_found(id: &str) -> ApiError` — code PROJECT_NOT_FOUND, status 404.
- `ApiError::project_ambiguous(msg: impl Into<String>) -> ApiError` — code PROJECT_AMBIGUOUS, status 409.
- `ApiError::project_id_conflict(msg: impl Into<String>) -> ApiError` — code PROJECT_ID_CONFLICT, status 409.
- `ApiError::projects_unavailable() -> ApiError` — code PROJECTS_UNAVAILABLE, status 500.
- `ApiError::pr_not_found(repo, pr_number) -> ApiError` — code PR_NOT_FOUND, status 404.
- `ApiError::pull_request_not_found(repo, pr_number) -> ApiError` — code PULL_REQUEST_NOT_FOUND, status 404.
- `ApiError::pull_request_project_mismatch(repo, pr_number, project_id) -> ApiError` — code PULL_REQUEST_PROJECT_MISMATCH, status 409.
- `ApiError::run_not_found(id: &str) -> ApiError` — code RUN_NOT_FOUND, status 404.
- `ApiError::active_run_not_found(loop_id: &str) -> ApiError` — code ACTIVE_RUN_NOT_FOUND, status 404.
- `ApiError::runtime_control_unavailable() -> ApiError` — code RUNTIME_CONTROL_UNAVAILABLE, status 501.
- `ApiError::agent_not_configured(loop_type: &str) -> ApiError` — code AGENT_NOT_CONFIGURED, status 400.

### 1.6 Handler Struct

```rust
pub struct Handler {
    context: Context,
    now: Box<dyn Fn() -> DateTime<Utc>>,
    recovery_summary: Box<dyn Fn() -> serde_json::Value>,
    webhook_forwarder: Option<Arc<dyn WebhookForwarder>>,
}
```

**Constructor**: `Handler::new(context: Context) -> Self`

Giải thích:
- `now`: mac dinh `Utc::now()`, co the override cho testing.
- `recovery_summary`: Go co 2 sources: `context.recovery_summary` (function) hoac `runtime.RecoverySummary()`.
- `webhook_forwarder`: Lay tu `context.webhook_forwarder` hoac `runtime.WebhookForwarder()`.

### 1.7 Context Struct (dependencies)

```rust
pub struct Context {
    pub config: Config,
    pub runtime: Arc<dyn RuntimeState>,
    pub webhook_forwarder: Option<Arc<dyn WebhookForwarder>>,
    pub projects_service: Option<Arc<dyn ProjectService>>,
    pub now: Option<Box<dyn Fn() -> DateTime<Utc>>>,
    pub recovery_summary: Option<Box<dyn Fn() -> serde_json::Value>>,
    pub reconcile_stale_runs: Option<Arc<dyn Fn(Context) -> Result<StaleRunReconcileSummary>>>,
    pub stop_loop: Option<Arc<dyn Fn(Context, String, String) -> Result<serde_json::Value>>>,
    pub close_loop: Option<Arc<dyn Fn(Context, String, String) -> Result<serde_json::Value>>>,
    pub stop_all: Option<Arc<dyn Fn(Context, String) -> Result<serde_json::Value>>>,
    pub repair_reviewer: Option<Arc<dyn Fn(RepairInput) -> Result<RepairResult>>>,
    pub trigger_scheduler_tick: Option<Box<dyn Fn()>>,
}
```

### 1.8 RuntimeState Trait (interface)

```rust
#[async_trait]
pub trait RuntimeState: Send + Sync {
    fn services(&self) -> Services;
    fn started_at(&self) -> Option<DateTime<Utc>>;

    // Optional methods (Go uses type assertion to check)
    fn recovery_summary(&self) -> Option<RecoverySummary> { None }
    fn webhook_forwarder(&self) -> Option<Arc<dyn WebhookForwarder>> { None }
    fn webhook_status(&self) -> Option<WebhookStatus> { None }
    fn record_webhook_delivery(&self, event_type: &str, delivery_id: &str) {}
    fn network_status(&self) -> Option<NetworkStatus> { None }
    fn worktree_cleanup_status(&self) -> Option<WorktreeCleanupStatus> { None }
    fn refresh_webhook_forwarders(&self) -> Result<()> { Ok(()) }
    fn reconcile_webhook_forwarders(&self) {}
}
```

**Note**: Trong Go, cac method optional nay duoc phat hien qua `any(h.context.Runtime).(interface{ ... })` type assertion. Trong Rust, dung default trait methods de dat duoc effect tuong tu (subclass co the override, Handler goi `.webhook_status()` thay vi `as_any().downcast_ref()`).

### 1.9 Server Struct

```rust
pub struct Server {
    config: Config,
    handler: Arc<dyn HandlerTrait>,  // impl Handler trait
    listener: Mutex<Option<TcpListener>>,
    server: Mutex<Option<tokio::net::TcpListener>>,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
}
```

**Methods**:
- `Server::new(config: Config, handler: impl HandlerTrait) -> Self`
- `async fn start(&self) -> Result<()>` — bind socket, spawn accept loop.
- `async fn stop(&self, ctx: Context)` — graceful shutdown.
- `fn addr(&self) -> Option<SocketAddr>` — lay dia chi dang listen.

**Go Server chi tiet**:
- `ReadHeaderTimeout: 30 * time.Second`
- Port config: `server.host` + `server.port`
- Hanh vi start idempotent: neu da start thi return nil.
- Stop graceful: `server.Shutdown(ctx)` + cho `<-done`.

### 1.10 ProjectService Trait

```rust
#[async_trait]
pub trait ProjectService: Send + Sync {
    async fn list(&self, ctx: Context) -> Result<Vec<ProjectRecord>>;
    async fn add_project(&self, ctx: Context, input: AddProjectInput) -> Result<AddProjectResult>;
    async fn remove_project(&self, ctx: Context, id: String) -> Result<ProjectRecord>;
}
```

---

## 2. Auth Middleware (Kiem tra quyen)

### 2.1 authorizeRequest Flow

```
function authorizeRequest(request, path, config) -> Result<(), ApiError>
```

**Step 1 — Loopback webhook bypass**:
- Neu `path == "/webhook/forward"` AND `config.webhook.enabled == true` AND `isLoopbackRemoteAddr(request.remote_addr) == true`:
  - Kiem tra forwarding proxy headers: `Forwarded`, `X-Forwarded-For`, `X-Forwarded-Host`, `X-Real-Ip`, `X-Real-IP`.
  - Neu KHONG co proxy headers (`hasForwardingProxyHeaders` tra false) → **allow (return Ok)**.
  - Neu co proxy headers → tiep tuc xuong step 2 (require Bearer token).

**Step 2 — Auth mode check**:
- Neu `config.server.auth_mode != "local-token"` → **allow (return Ok)** (unrestricted mode).

**Step 3 — Token configuration check**:
- Neu `config.server.local_token` la nil hoac empty → return `ApiError(AUTH_MISCONFIGURED, 500, "Local token auth is enabled but no token is configured")`.

**Step 4 — Bearer token match**:
- Neu `Authorization` header != `"Bearer {token}"` → return `ApiError(UNAUTHORIZED, 401, "Authorization token is required")`.

**Step 5 — Allow**:
- Match thanh cong → **allow (return Ok)**.

### 2.2 Loopback Detection Functions

**`isLoopbackRemoteAddr(remote_addr: &str) -> bool`**:
1. Trim whitespace.
2. Neu empty → false.
3. Parse `SplitHostPort` (handle `[ip]:port`, `ip:port`).
4. Strip brackets `[]` tu IPv6.
5. Neu host == "localhost" (case-insensitive) → true.
6. Parse IP, tra ve `ip.is_loopback()`.

**`isLoopbackRequest(request) -> bool`**:
- Doc `request.remote_addr`, split host/port, parse IP, tra ve `ip.is_loopback()`. (Cung logic nhu `isLoopbackRemoteAddr` nhung khong check "localhost" string.)

**Note**: `isLoopbackRequest` duoc dung rieng trong `buildWebhookForwardResponse` de check sau auth, trong khi `isLoopbackRemoteAddr` duoc dung trong `authorizeRequest`.

### 2.3 hasForwardingProxyHeaders

```rust
fn has_forwarding_proxy_headers(headers: &HeaderMap) -> bool {
    let proxy_headers = ["Forwarded", "X-Forwarded-For", "X-Forwarded-Host", "X-Real-Ip", "X-Real-IP"];
    for name in proxy_headers {
        if let Some(values) = headers.get_all(name).iter().next() {
            if !values.to_str().unwrap_or("").trim().is_empty() {
                return true;
            }
        }
    }
    false
}
```

### 2.4 Auth Constants

```rust
pub enum AuthMode {
    None,           // "none" — unrestricted
    LocalToken,     // "local-token" — Bearer token required
}

impl AuthMode {
    pub fn from_str(s: &str) -> Result<Self>;  // validate: only these 2 values
}
```

Default config value: `AuthMode::None`.

### 2.5 Route-Specific Access Summary

| Route | Auth Requirement |
|-------|-----------------|
| `POST /webhook/forward` | Loopback-only (127.0.0.1, ::1, localhost). Neu co proxy headers → Bearer token required. |
| All `GET/POST /api/v1/*` | Bearer token required khi `auth_mode == local-token`. Unrestricted khi `auth_mode == none`. |

---

## 3. API Endpoints (Day du 31+ endpoints)

### 3.1 Static Routes (exact path match)

| # | Method | Path | Handler | Auth | Description |
|---|--------|------|---------|------|-------------|
| 1 | POST | `/webhook/forward` | `build_webhook_forward_response` | Loopback + Bearer | Forward GitHub webhook payload. |
| 2 | GET | `/api/v1/healthz` | `build_health_response` | Bearer/none | Storage + migration health. |
| 3 | GET | `/api/v1/status` | `build_status_response` | Bearer/none | Full runtime status dump. |
| 4 | GET | `/api/v1/version` | `build_version_response` | Bearer/none | Version + binary info. |
| 5 | GET | `/api/v1/config` | `build_config_response` | Bearer/none | Live config (token redacted). |
| 6 | GET | `/api/v1/webhook/status` | `build_webhook_status_response` | Bearer/none | Webhook forwarder stats. |
| 7 | GET | `/api/v1/events` | `build_events_route_response` | Bearer/none | Paginated event log. |
| 8 | GET | `/api/v1/pull-requests` | `build_pull_requests_route_response` | Bearer/none | All tracked PRs + loop status. |
| 9 | GET | `/api/v1/projects` | `build_projects_route_response` (GET/switch) | Bearer/none | List projects. |
| 10 | POST | `/api/v1/projects` | `build_projects_route_response` (POST/switch) | Bearer/none | Create project. |
| 11 | GET | `/api/v1/loops` | `build_loops_route_response` (GET/switch) | Bearer/none | List loops. |
| 12 | POST | `/api/v1/loops` | `build_loops_route_response` (POST/switch) | Bearer/none | Create loop. |
| 13 | POST | `/api/v1/workers` | `build_workers_create_response` | Bearer/none | Create worker loop. |
| 14 | POST | `/api/v1/planners` | `build_planners_create_response` | Bearer/none | Create planner loop. |
| 15 | GET | `/api/v1/runs` | `build_runs_route_response` | Bearer/none | List runs (query: `?loopId=`). |
| 16 | POST | `/api/v1/runs/reconcile-stale` | `build_reconcile_stale_runs_response` | Bearer/none | Trigger stale run reconciliation. |
| 17 | GET | `/api/v1/runs/active` | `build_active_runs_response` | Bearer/none | Active runs (query: `?all=`, `?status=`, `?type=`, `?projectId=`, `?repo=`, `?prNumber=`). |
| 18 | GET | `/api/v1/reviewer/repair` | `build_reviewer_repair_route_response` | Bearer/none | Repair reviewer state. |

### 3.2 Dynamic Routes (prefix match)

| # | Method | Path | Handler | Auth | Description |
|---|--------|------|---------|------|-------------|
| 19 | GET | `/api/v1/loops/{id}` | `build_loop_route_response` | Bearer/none | Loop detail (by ID or seq). |
| 20 | GET | `/api/v1/loops/{id}/logs` | `build_loop_logs_response` | Bearer/none | Latest loop logs (snapshot). |
| 21 | GET | `/api/v1/loops/{id}/logs?follow=1` | `stream_loop_logs_route` | Bearer/none | SSE stream of loop logs. |
| 22 | POST | `/api/v1/loops/{id}/start` | `mutate_loop_status(..Running)` | Bearer/none | Start/pause loop. |
| 23 | POST | `/api/v1/loops/{id}/pause` | `mutate_loop_status(..Paused)` | Bearer/none | Pause loop. |
| 24 | POST | `/api/v1/loops/{id}/retry` | `retry_loop` | Bearer/none | Retry loop (body: `{mode, resetAttempts}`). |
| 25 | DELETE | `/api/v1/projects/{id}` | `build_project_route_response` | Bearer/none | Remove project. |
| 26 | GET | `/api/v1/events/{entityType}/{id}` | `build_entity_events_route_response` | Bearer/none | Events for specific entity. |
| 27 | GET | `/api/v1/pull-requests/{repo}/{prNumber}` | `build_pull_request_route_response` | Bearer/none | Single PR detail with loop status. |
| 28 | GET | `/api/v1/pull-requests/{repo}/{prNumber}/status` | `build_pull_request_status_response` | Bearer/none | PR status summary (lighter). |
| 29 | GET | `/api/v1/runs/active/{loopId}` | `build_active_run_route_response` | Bearer/none | Active run for a loop (by ID or seq). |
| 30 | POST | `/api/v1/runs/active/{loopId}/stop` | `build_active_run_route_response` | Bearer/none | Stop loop. |
| 31 | POST | `/api/v1/runs/active/{loopId}/close` | `build_active_run_route_response` | Bearer/none | Close loop. |
| 32 | POST | `/api/v1/runs/active/stop-all` | `build_active_run_route_response` | Bearer/none | Stop all loops. |
| 33 | GET | `/api/v1/runs/{id}/logs` | `build_run_logs_response` | Bearer/none | Logs for specific run. |

**Note**: Cac sub-resource nhu `loops/{id}/logs`, `loops/{id}/start`, `loops/{id}/pause`, `loops/{id}/retry` duoc dispatch trong `build_loop_route_response` dua vao `parts[1]`. Tuong tu cho `runs/active/{selector}/stop`, `runs/active/{selector}/close`, `runs/active/stop-all` trong `build_active_run_route_response`.

### 3.3 Routing Dispatch Logic (Go switch -> Rust)

**Static route matching**: Exact match (method switch trong handler body neu can):
- `/webhook/forward` — only POST, checked in handler (ko dung `assertMethod`).
- `/api/v1/healthz` — assert GET, else 405.
- All others: assert method, build response.

**Prefix matching** (after static routes fail):
```
path starts with /api/v1/loops/   → buildLoopRouteResponse or streamLoopLogsRoute
path starts with /api/v1/projects/ → buildProjectRouteResponse
path starts with /api/v1/events/  → buildEntityEventsRouteResponse
path starts with /api/v1/pull-requests/ → buildPullRequestRouteResponse
path starts with /api/v1/runs/active/ → buildActiveRunRouteResponse
path starts with /api/v1/runs/    → buildRunRouteResponse (catch-all for /api/v1/runs/{id}/logs)
```

**Order matters**: `/api/v1/runs/active/` phai check truoc `/api/v1/runs/` vi prefix overlap. Tuong tu `/api/v1/runs/reconcile-stale` va `/api/v1/runs/active` la static routes truoc khi prefix matching.

**Unknown route fallback**: 404 ROUTE_NOT_FOUND.

---

## 4. Handler Function Signatures

### 4.1 Webhook Endpoint

```rust
// POST /webhook/forward
async fn build_webhook_forward_response(&self, req: &HttpRequest) -> Result<ForwardResult, ApiError>;
// Checks: POST method, loopback, webhook forwarder not nil, webhook runtime enabled
// Reads: X-GitHub-Delivery, X-GitHub-Event headers, body (1MB limit)
// On accept: calls runtime.RecordWebhookDelivery(event_type, delivery_id)

#[derive(Serialize)]
pub struct ForwardResult {
    pub status: String,     // "accepted" | "ignored" | "duplicate"
    pub reason: Option<String>,
    pub work_items: i32,
}
```

### 4.2 Health Endpoint

```rust
// GET /api/v1/healthz
async fn build_health_response(&self, ctx: Context) -> Result<HealthResponse, ApiError>;

#[derive(Serialize)]
pub struct HealthResponse {
    pub healthy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,  // ISO format
    pub storage: StorageHealth,
}

#[derive(Serialize)]
pub struct StorageHealth {
    pub ok: bool,
    pub mode: String,
    pub db_path: String,
    pub last_updated_at: String,     // ISO
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    pub migration: MigrationHealth,
}

#[derive(Serialize)]
pub struct MigrationHealth {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_available_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_applied_id: Option<String>,
    pub pending_count: i32,
}
```

### 4.3 Status Endpoint

```rust
// GET /api/v1/status
async fn build_status_response(&self, ctx: Context) -> Result<StatusResponse, ApiError>;

#[derive(Serialize)]
pub struct StatusResponse {
    pub service: StatusService,
    pub storage: StatusStorage,
    pub scheduler: StatusScheduler,
    pub agent: StatusAgent,
    pub worktree_cleanup: Option<serde_json::Value>,
    pub webhook: StatusWebhookSummary,
    pub loops: StatusLoops,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<serde_json::Value>,
    pub safety: StatusSafety,
    pub notifications: StatusNotifications,
    pub tools: StatusTools,
}
```

Cac sub-structures:
- `StatusService`: healthy, version, build, daemon_mode, started_at, recovery, binary
- `StatusBinary`: name ("looperd"), path, install_dir (~/.looper/bin), current_target, artifact_name, supported_targets
- `StatusStorage`: mode, db_path, schema_version, pending_migrations, healthy
- `StatusScheduler`: healthy, queued_items, running_items, completed_items, failed_items, total_runs, active_runs
- `StatusAgent`: vendor, model, native_resume_enabled, timeouts (planner/worker/reviewer/fixer each with idle_timeout_seconds + max_runtime_seconds)
- `StatusWebhookSummary`: enabled, endpoint_url, fallback_poll_interval_seconds, degraded, degraded_reasons, configured_forwarders, running_forwarders
- `StatusLoops`: planner/reviewer/worker/fixer each with queued/running/waiting/paused/failed/terminated/stopped counts
- `StatusSafety`: allow_auto_commit, allow_auto_push, allow_auto_approve, allow_risky_fixes, fix_all_pull_requests, open_pr_strategy
- `StatusNotifications`: in_app_enabled, osascript_enabled
- `StatusTools`: git, gh, osascript (bool: duong dan da config)

### 4.4 Version Endpoint

```rust
// GET /api/v1/version
fn build_version_response(&self) -> VersionResponse;

#[derive(Serialize)]
pub struct VersionResponse {
    pub version: String,
    pub build: BuildMetadata,
    pub binary: VersionBinaryResponse,
}

#[derive(Serialize)]
pub struct VersionBinaryResponse {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}
```

### 4.5 Config Endpoint

```rust
// GET /api/v1/config
fn build_config_response(&self) -> ConfigResponse;

#[derive(Serialize)]
pub struct ConfigResponse {
    pub server: ConfigServerResponse,
    pub storage: StorageConfig,
    pub scheduler: SchedulerConfig,
    pub webhook: WebhookConfig,
    pub agent: AgentConfig,
    pub logging: LoggingConfig,
    pub notifications: NotificationConfig,
    pub tools: ToolPathsConfig,
    pub daemon: ConfigDaemonResponse,
    pub package: PackageConfig,
    pub defaults: DefaultsConfig,
    pub roles: RoleConfigs,
    pub projects: Vec<ProjectRefConfig>,
}

#[derive(Serialize)]
pub struct ConfigServerResponse {
    pub host: String,
    pub port: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub auth_mode: String,
    pub local_token_configured: bool,  // true if non-empty token exists
}

#[derive(Serialize)]
pub struct ConfigDaemonResponse {
    pub mode: String,
    pub restart_policy: String,
    pub restart_throttle_seconds: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plist_path: Option<String>,
    pub log_dir: String,
    pub working_directory: String,
    pub environment: HashMap<String, String>,
    pub worktree_cleanup: WorktreeCleanupConfig,
}
```

### 4.6 Webhook Status Endpoint

```rust
// GET /api/v1/webhook/status
fn build_webhook_status_response(&self) -> WebhookStatus;

#[derive(Serialize)]
pub struct WebhookStatus {
    pub enabled: bool,
    pub mode: String,
    pub fallback_poll_interval_seconds: i32,
    pub listener_path: String,              // "/webhook/forward"
    pub endpoint_url: String,                // base_url + "/webhook/forward"
    pub tunnel_public_base_url: String,
    pub degraded: bool,
    pub degraded_reasons: Vec<String>,
    pub recent_outcomes: Vec<WebhookRecentOutcome>,
    pub forwarders: Vec<WebhookForwarderState>,
}
```

### 4.7 Events Endpoint

```rust
// GET /api/v1/events?limit=N
async fn build_events_route_response(&self, req: &HttpRequest) -> Result<EventsListResponse, ApiError>;

#[derive(Serialize)]
pub struct EventsListResponse {
    pub items: Vec<EventResponse>,
}

#[derive(Serialize)]
pub struct EventResponse {
    pub id: String,
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor_display_name: Option<String>,
    pub payload_json: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,  // parsed from payload_json
}
```

**Query params**: `?limit=N` — positive int, default 100.

### 4.8 Entity Events Endpoint

```rust
// GET /api/v1/events/{entityType}/{entityId}
async fn build_entity_events_route_response(&self, req: &HttpRequest, path: &str) -> Result<EntityEventsResponse, ApiError>;

#[derive(Serialize)]
pub struct EntityEventsResponse {
    pub entity_type: String,
    pub entity_id: String,
    pub items: Vec<EventResponse>,
}
```

**Path parsing**: `{entityType}` va `{entityId}` tu path segments, URL-decoded. Neu co segment thu 3 khac empty → 404.

### 4.9 Pull Requests Endpoint

```rust
// GET /api/v1/pull-requests
async fn build_pull_requests_route_response(&self, req: &HttpRequest) -> Result<PullRequestsListResponse, ApiError>;

#[derive(Serialize)]
pub struct PullRequestsListResponse {
    pub items: Vec<PullRequestResponse>,
}

#[derive(Serialize)]
pub struct PullRequestResponse {
    pub repo: String,
    pub pr_number: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks_summary: Option<String>,
    pub unresolved_thread_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mergeability: Option<String>,        // "ready" | "waiting" | "blocked" | "draft" | "unknown"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking_reason: Option<String>,     // "" | "conflicts" | "checks" | "checks pending" | "review" | "review pending" | "draft" | "no snapshot"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_draft: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_conflicts: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,           // latest loop status for reviewer
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixer: Option<String>,              // latest loop status for fixer
}
```

### 4.10 Pull Request Detail Endpoint

```rust
// GET /api/v1/pull-requests/{repo}/{prNumber}
// GET /api/v1/pull-requests/{repo}/{prNumber}/status
async fn build_pull_request_route_response(&self, req: &HttpRequest, path: &str) -> Result<PullRequestResponse, ApiError>;

// GET /api/v1/pull-requests/{repo}/{prNumber}/status (sub-resource)
async fn build_pull_request_status_response(&self, ctx: Context, snapshot: PullRequestSnapshotRecord) -> Result<PullRequestStatusResponse, ApiError>;

#[derive(Serialize)]
pub struct PullRequestStatusResponse {
    pub repo: String,
    pub pr_number: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks_summary: Option<String>,
    pub unresolved_thread_count: i64,
    pub captured_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixer: Option<String>,
    pub loop_status: PullRequestLoopStatus,
}

#[derive(Serialize)]
pub struct PullRequestLoopStatus {
    pub loops: Vec<String>,            // loop statuses
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_run_status: Option<String>,
    pub running_run_count: i32,
}
```

### 4.11 Projects Endpoints

```rust
// GET /api/v1/projects (list) + POST /api/v1/projects (create)
async fn build_projects_route_response(&self, req: &HttpRequest) -> Result<Value, ApiError>;
// Dispatch by method: GET -> list, POST -> create

// DELETE /api/v1/projects/{id}
async fn build_project_route_response(&self, req: &HttpRequest, path: &str) -> Result<serde_json::Value, ApiError>;

#[derive(Serialize)]
pub struct ProjectsListResponse {
    pub items: Vec<ProjectResponse>,
}

#[derive(Serialize)]
pub struct ProjectResponse {
    pub id: String,
    pub name: String,
    pub repo_path: String,
    pub base_branch: String,
    pub archived: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_root: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize)]
pub struct CreateProjectResponse {
    #[serde(flatten)]
    pub project: ProjectResponse,
    pub discovered_pull_requests: i32,
    pub discovered_worktrees: i32,
    pub pending_snapshots: i32,
    pub captured_snapshots: i32,
    pub warnings: Vec<String>,
}
```

**Create project request body**:
```json
{
  "repoPath": "/full/path/to/repo",
  "id": "optional-custom-id",
  "name": "Optional display name",
  "baseBranch": "main",
  "worktreeRoot": "/optional/worktree/root",
  "repo": "owner/repo",
  "snapshotMode": "async|full|off"
}
```

**Error mapping for create**:
- `ProjectIDCollisionError` → 409 PROJECT_ID_CONFLICT
- "invalid project id" prefix → 400 VALIDATION_FAILED
- Other → 500 INTERNAL_ERROR

### 4.12 Loops Endpoint

```rust
// GET /api/v1/loops (list) + POST /api/v1/loops (create)
async fn build_loops_route_response(&self, req: &HttpRequest) -> Result<LoopsListResponse, ApiError>;

// GET /api/v1/loops/{id} (detail) + sub-resources (logs, start, pause, retry)
async fn build_loop_route_response(&self, req: &HttpRequest, path: &str) -> Result<serde_json::Value, ApiError>;

#[derive(Serialize)]
pub struct LoopsListResponse {
    pub items: Vec<LoopResponse>,
}

#[derive(Serialize)]
pub struct LoopResponse {
    pub id: String,
    pub seq: i64,
    pub project_id: String,
    #[serde(rename = "type")]
    pub loop_type: String,          // "planner" | "reviewer" | "worker" | "fixer"
    pub target_type: String,        // "project" | "pull_request" | "issue"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<i64>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
```

**Loop sub-resource dispatch** (inside `build_loop_route_response`):
```
"logs"  → GET  build_loop_logs_response (snapshot)
"start" → POST mutate_loop_status(..Running)
"pause" → POST mutate_loop_status(..Paused)
"retry" → POST retry_loop
default → 404 ROUTE_NOT_FOUND
```

**SSE streaming** (separate path check, before `build_loop_route_response`):
- Path: `/api/v1/loops/{id}/logs?follow=1` or `?follow=true`
- Handler: `stream_loop_logs_route` -> `stream_loop_logs`

### 4.13 Loop Logs Response

```rust
#[derive(Serialize)]
pub struct LoopLogsResponse {
    pub seq: i64,
    pub loop_id: String,
    pub loop_type: String,
    pub loop_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<LoopLogsRunResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<LoopLogsAgentPayload>,
}

#[derive(Serialize)]
pub struct LoopLogsRunResponse {
    pub run_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_step: Option<String>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

#[derive(Serialize)]
pub struct LoopLogsAgentPayload {
    pub execution_id: String,
    pub vendor: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i64>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    pub heartbeat_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_heartbeat_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub stdout: String,
    pub stderr: String,
}
```

**Log file reading**: Go doc stdout/stderr tu `outputJSON` field. Neu co `stdoutLogPath`/`stderrLogPath`, doc tu file (max 16MB, chi doc 16MB cuoi cung). Kiem tra `isPathWithinDirectory(path, logDir)` de prevent path traversal.

### 4.14 Workers Create Endpoint

```rust
// POST /api/v1/workers
async fn build_workers_create_response(&self, req: &HttpRequest) -> Result<WorkerCreateResponse, ApiError>;

#[derive(Deserialize)]
pub struct CreateWorkerRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_number: Option<i64>,
}

#[derive(Serialize)]
pub struct WorkerCreateResponse {
    #[serde(flatten)]
    pub loop: LoopResponse,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_path: Option<String>,
    pub base_branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_number: Option<i64>,
    #[serde(skip_serializing_if = "serde_json::Value::is_null", default)]
    pub reused: bool,
}
```

**Validation rules**:
- `prompt`/`specPath` vs `prNumber` vs `issueNumber`: exactly 1 input mode (mutually exclusive).
- `project_id` or resolveable via `repo` + `prNumber` or `repo` alone.
- `repo` required (tu body hoac project metadata).
- `base_branch` required (tu body, project, hoac defaults).

### 4.15 Planners Create Endpoint

```rust
// POST /api/v1/planners
async fn build_planners_create_response(&self, req: &HttpRequest) -> Result<PlannerCreateResponse, ApiError>;

#[derive(Deserialize)]
pub struct CreatePlannerRequest {
    pub project_id: Option<String>,
    pub issue_number: Option<i64>,
}

#[derive(Serialize)]
pub struct PlannerCreateResponse {
    #[serde(flatten)]
    pub loop: LoopResponse,
    pub issue_number: i64,
}
```

### 4.16 Runs Endpoints

```rust
// GET /api/v1/runs
async fn build_runs_route_response(&self, req: &HttpRequest) -> Result<RunsListResponse, ApiError>;

#[derive(Serialize)]
pub struct RunsListResponse {
    pub items: Vec<RunResponse>,
}

#[derive(Serialize)]
pub struct RunResponse {
    pub id: String,
    pub loop_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_step: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_completed_step: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_heartbeat_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
```

**Query**: `?loopId=<id>` — filter runs by loop.

```rust
// POST /api/v1/runs/reconcile-stale
async fn build_reconcile_stale_runs_response(&self, req: &HttpRequest) -> Result<StaleRunReconcileSummary, ApiError>;

// GET /api/v1/runs/active
async fn build_active_runs_response(&self, req: &HttpRequest) -> Result<ActiveRunsListResponse, ApiError>;

#[derive(Serialize)]
pub struct ActiveRunsListResponse {
    pub items: Vec<ActiveRunView>,
}

// GET /api/v1/runs/{id}/logs
async fn build_run_logs_response(&self, ctx: Context, run_id: &str) -> Result<LoopLogsResponse, ApiError>;
```

### 4.17 Active Run View

```rust
#[derive(Serialize)]
pub struct ActiveRunView {
    pub seq: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub loop_id: String,
    pub project_id: String,
    #[serde(rename = "type")]
    pub loop_type: String,
    pub status: String,
    pub loop_status: String,
    pub display_status: String,   // enriched: "manual_intervention" | "backing_off" | same as status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_step: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    pub target: ActiveRunTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<ActiveRunAgent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<ActiveRunWorktree>,
}

#[derive(Serialize)]
pub struct ActiveRunTarget {
    #[serde(rename = "type")]
    pub target_type: String,       // "project" | "pull_request" | "issue"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_number: Option<i64>,
    pub label: String,             // human-readable: name/ "owner/repo#N"
}

#[derive(Serialize)]
pub struct ActiveRunAgent {
    pub active: bool,
    pub active_count: i32,
    pub execution_id: String,
    pub vendor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i64>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_heartbeat_at: Option<String>,
    pub heartbeat_count: i64,
    pub status: String,
}

#[derive(Serialize)]
pub struct ActiveRunWorktree {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

#[derive(Deserialize)]
pub struct ActiveRunsQuery {
    pub all: Option<bool>,          // ?all=true - include inactive
    pub status: Option<String>,     // ?status=running
    #[serde(rename = "type")]
    pub loop_type: Option<String>,  // ?type=reviewer
    pub project_id: Option<String>, // ?projectId=xxx
    pub repo: Option<String>,       // ?repo=owner/repo
    pub pr_number: Option<i64>,     // ?prNumber=123
}
```

**Validation**: `repo` va `prNumber` must be provided together.

### 4.18 Active Run Route (multi-dispatch)

```rust
// GET/POST /api/v1/runs/active/{selector}[/{action}]
// GET /api/v1/runs/active/stop-all  (POST)
async fn build_active_run_route_response(&self, req: &HttpRequest, path: &str) -> Result<serde_json::Value, ApiError>;
```

**Dispatch logic**:
```
parts[0] == "stop-all" && len(parts) == 1:
  POST → context.stop_all("Stopped by user via selector all")
  else → 405

len(parts) == 1 || parts[1] empty:
  GET → build_active_run_detail_response (resolve loop by selector)

parts[1] == "stop":
  POST → context.stop_loop(loop_id, "Stopped by user via selector {selector}")
  else → 405

parts[1] == "close":
  POST → context.close_loop(loop_id, "Closed by user via selector {selector}")
  else → 405

default → 404
```

### 4.19 Reviewer Repair Endpoint

```rust
// GET /api/v1/reviewer/repair
// Handler: build_reviewer_repair_route_response
async fn build_reviewer_repair_route_response(&self, req: &HttpRequest) -> Result<RepairResult, ApiError>;
```

**Logic** (tu `internal/api/reviewer_repair.go`):
- Kiem tra method GET, parse query params: `projectId`, `repo`, `prNumber`, `loopIds` (comma-separated).
- Goi `context.repair_reviewer(RepairInput { ... })`.
- Tra ve `RepairResult`.

**RepairInput**:
```rust
pub struct RepairInput {
    pub project_id: Option<String>,
    pub repo: Option<String>,
    pub pr_number: Option<i64>,
    pub loop_ids: Vec<String>,
}
```

---

## 5. SSE Streaming Pattern (Server-Sent Events)

### 5.1 Channel Subscription Model

**Flow**:
1. Client gui `GET /api/v1/loops/{id}/logs?follow=1`.
2. Server set headers:
   - `Content-Type: text/event-stream`
   - `Cache-Control: no-cache`
   - `Connection: keep-alive`
   - `X-Request-ID: {request_id}`
3. Server gui event `snapshot` voi payload `LoopLogsResponse` hien tai.
4. Server poll storage every `200ms` (`loopLogsFollowPollInterval`).
5. Tren moi poll, tinh toan log chunk moi, gui event `chunk`.
6. Khi loop run ket thuc, gui event `end` voireason.

**SSE event format**:
```
event: snapshot
data: {"seq":123,...}

event: chunk
data: {"runId":"...","currentStep":"...","executionId":"...","vendor":"claude","pid":12345,"status":"running","content":"new log text"}

event: end
data: {"reason":"run_completed"}
```

### 5.2 Chunk Calculation Logic

```rust
fn appended_log_chunk(
    previous_execution_id: &str,
    previous_content: &str,
    current_execution_id: &str,
    current_content: &str,
) -> String {
    if current_execution_id.is_empty() {
        return String::new();
    }
    if previous_execution_id.is_empty() || current_execution_id != previous_execution_id {
        return current_content.to_string();  // new execution: send full content
    }
    if current_content == previous_content {
        return String::new();  // no change
    }
    if current_content.starts_with(previous_content) {
        // Normal case: content appended
        return current_content[previous_content.len()..].to_string();
    }
    // Content changed in middle: send full content
    current_content.to_string()
}
```

### 5.3 Termination Conditions

**`should_terminate_loop_logs_follow(resp, observed_run_id)`**:
```
if observed_run_id is empty:
  if resp.run is None:
    return !is_active_loop_status(resp.loop_status)
  observed_run_id = resp.run.run_id
if resp.run is None: return true
if resp.run.run_id != observed_run_id: return true
return is_terminal_run_status(resp.run.status)
```

**`should_terminate_loop_logs_follow_before_chunk(resp, observed_run_id)`**:
- Chi terminate `before_chunk` neu run khong con ton tai (da chuyen sang run moi). Neu same runID nhung terminal, cho phep chunk cuoi cung.

**Stream state**:
- `stderr` query param: `?stderr=1` hoac `?stderr=true` de xem stderr thay vi stdout.
- Auto-detect: neu stdout empty va stderr non-empty, mac dinh xem stderr.

### 5.4 Client Disconnect Handling

- Poll loop dung `select` de listen `request.Context().Done()`.
- Khi client disconnect, `ctx.done()` channel close → handler return ko error.
- Go pattern:
  ```go
  select {
  case <-r.Context().Done():
      return nil
  case <-ticker.C:
  }
  ```

### 5.5 Backpressure Strategy

- **Poll-based**: khong co backpressure channel. Server poll storage every 200ms, chi gui chunk khi co content moi.
- **No buffer**: neu client doc cham, TCP backpressure tu nhien. Server block tren `Flush()`.
- **Flusher check**: `w.(http.Flusher)` — fallback 500 internal_error neu response writer ko support Flush.
- **Error on write**: neu `writeSSEEvent` fail (client gone), handler silently return nil (no error to caller).

### 5.6 SSE Write Helper

```rust
fn write_sse_event(
    writer: &mut impl Write,
    flusher: &impl Flush,
    event: &str,
    payload: &impl Serialize,
) -> Result<(), Error> {
    let encoded = serde_json::to_string(payload)?;
    write!(writer, "event: {}\ndata: {}\n\n", event, encoded)?;
    flusher.flush()?;
    Ok(())
}
```

---

## 6. Error Response Patterns

### 6.1 Envelope Format

**Success response**:
```json
{
  "ok": true,
  "data": { ... },
  "requestId": "req_abc123"
}
```

**Error response**:
```json
{
  "ok": false,
  "error": {
    "code": "ROUTE_NOT_FOUND",
    "message": "Unknown route: /api/v1/foo"
  },
  "requestId": "req_abc123"
}
```

**Error with details**:
```json
{
  "ok": false,
  "error": {
    "code": "VALIDATION_FAILED",
    "message": "prNumber must be a positive integer",
    "details": {
      "field": "prNumber",
      "received": "abc"
    }
  },
  "requestId": "req_abc123"
}
```

### 6.2 All 20 Error Codes and When They Fire

| Error Code | HTTP Status | Trigger Condition |
|---|---|---|
| `ACTIVE_RUN_NOT_FOUND` | 404 | Active run khong tim thay cho loop ID cu the |
| `AGENT_NOT_CONFIGURED` | 400 | Tao/retry loop nhung `config.agent.vendor` khong config |
| `AUTH_MISCONFIGURED` | 500 | Auth mode = local-token nhung token trong/empty |
| `INTERNAL_ERROR` | 500 | Unexpected error (DB fail, parse fail, nil pointer) |
| `LOOP_CONFLICT` | 409 | Loop da ton tai cho project+type+target key |
| `LOOP_NOT_FOUND` | 404 | Loop ID hoac seq khong ton tai |
| `METHOD_NOT_ALLOWED` | 405 | HTTP method khong hop le cho route |
| `PROJECTS_UNAVAILABLE` | 500 | Project management service khong available trong runtime |
| `PROJECT_AMBIGUOUS` | 409 | Nhieu project match cung repo/PR, can explicit projectId |
| `PROJECT_ID_CONFLICT` | 409 | Project ID da ton tai khi create |
| `PROJECT_NOT_FOUND` | 404 | Project ID khong ton tai hoac archived |
| `PR_NOT_FOUND` | 404 | Pull request snapshot khong tim thay |
| `PULL_REQUEST_NOT_FOUND` | 404 | PR khong thuoc project cu the |
| `PULL_REQUEST_PROJECT_MISMATCH` | 409 | PR thuoc project khac, hoac repo request ko match project |
| `ROUTE_NOT_FOUND` | 404 | URL path khong match bat ky route nao |
| `RUN_NOT_FOUND` | 404 | Run ID khong ton tai |
| `RUNTIME_CONTROL_UNAVAILABLE` | 501 | Stop/reconcile/reviewer repair runtime callbacks khong duoc set |
| `UNAUTHORIZED` | 401 | Bearer token khong match hoac webhook loopback fail |
| `VALIDATION_FAILED` | 400 | Input validation fail (missing field, invalid format, etc.) |

### 6.3 Additional Error Codes Referenced in Code (extended enum)

The Go codebase also defines these in `pkg/api/envelope.go` but they are less common:
- Already fully listed above (20 codes total).

### 6.4 Webhook Forward Error Mapping

| Error Condition | HTTP Status | Error Code |
|---|---|---|
| Method != POST | 405 | METHOD_NOT_ALLOWED |
| Not loopback caller | 403 | UNAUTHORIZED (note: 403 not 401 in this case) |
| Webhook forwarder nil | 500 | INTERNAL_ERROR |
| Webhook runtime disabled | 503 | INTERNAL_ERROR |
| Body read error | 500 | INTERNAL_ERROR |
| "not configured" in error | 500 | INTERNAL_ERROR |
| "queue is full" in error | 503 | INTERNAL_ERROR |
| Other forward errors | 400 | VALIDATION_FAILED |

### 6.5 Response Writing

```rust
fn write_success<T: Serialize>(w: &mut HttpResponse, request_id: &str, data: T) {
    write_json(w, StatusCode::OK, Envelope::success(request_id, data));
}

fn write_error(w: &mut HttpResponse, request_id: &str, err: ApiError) {
    let envelope = Envelope::failure(request_id, err.code, &err.message, err.details);
    write_json(w, err.status, envelope);
}

fn write_json<T: Serialize>(w: &mut HttpResponse, status: StatusCode, payload: T) {
    w.headers_mut().insert("content-type", "application/json; charset=utf-8".parse().unwrap());
    *w.status_mut() = status;
    serde_json::to_writer(w, &payload).ok();
}
```

### 6.6 Request ID Extraction

```rust
fn extract_request_id(headers: &HeaderMap) -> String {
    match headers.get("x-request-id") {
        Some(val) => {
            let trimmed = val.to_str().unwrap_or("").trim().to_string();
            if trimmed.is_empty() {
                generate_request_id()
            } else {
                trimmed
            }
        }
        None => generate_request_id(),
    }
}
```

---

## 7. Webhook Event Routing (Tu forwarder.go)

### 7.1 Forwarder struct

```rust
pub struct WebhookForwarder {
    repos: Arc<Repositories>,
    cfg: Config,
    reviewer: Arc<dyn TargetedReviewer>,
    fixer: Arc<dyn TargetedFixer>,
    logger: Option<Box<dyn Logger>>,
    now: Box<dyn Fn() -> DateTime<Utc>>,

    // Configurable via Options
    queue_capacity: usize,        // default: 128
    max_concurrent: usize,        // default: 4
    delivery_ttl: Duration,       // default: 1 hour
    retry_delay: Duration,        // default: 2 seconds
    recent_outcome_limit: usize,  // default: 64

    // Internal state (Mutex-protected)
    inner: Mutex<ForwarderInner>,
}

struct ForwarderInner {
    closed: bool,
    queue: Vec<WorkKey>,
    works: HashMap<String, WorkItem>,
    deliveries: HashMap<String, DeliveryRecord>,
    stats: Stats,
    recent_outcomes: Vec<Outcome>,
    current_in_flight: i32,
}
```

### 7.2 Key Types

```rust
#[derive(Clone, Hash, Eq, PartialEq)]
pub struct WorkKey {
    pub project_id: String,
    pub repo: String,
    pub object_type: String,   // "pull_request" | "base_branch"
    pub number: i64,           // PR number (0 for base_branch)
    pub branch: String,        // branch name (empty for pull_request)
}

pub struct WorkItem {
    pub key: WorkKey,
    pub lanes: HashSet<Lane>,
    pub metadata: WorkMetadata,
    pub running: bool,
    pub enqueued: bool,
}

pub struct WorkMetadata {
    pub event_type: String,
    pub action: String,
    pub delivery_id: String,
}

pub enum Lane {
    Reviewer,
    Fixer,
}

pub struct DeliveryRequest {
    pub delivery_id: String,
    pub event_type: String,
    pub payload: Vec<u8>,
}

pub struct ForwardResult {
    pub status: String,      // "accepted" | "ignored" | "duplicate"
    pub reason: String,
    pub work_items: i32,
}
```

### 7.3 Webhook Envelope Types (JSON)

```rust
#[derive(Deserialize)]
pub struct PushEnvelope {
    pub r#ref: String,
    pub deleted: bool,
    pub repository: RepositoryRef,
}

#[derive(Deserialize)]
pub struct PullRequestEnvelope {
    pub action: String,
    pub repository: RepositoryRef,
    pub pull_request: PullRequestRef,
}

#[derive(Deserialize)]
pub struct IssueCommentEnvelope {
    pub action: String,
    pub repository: RepositoryRef,
    pub issue: IssueRef,
}

#[derive(Deserialize)]
pub struct CheckRunEnvelope {
    pub action: String,
    pub repository: RepositoryRef,
    pub check_run: CheckRunBody,
}

#[derive(Deserialize)]
pub struct RepositoryRef {
    pub full_name: String,
}

#[derive(Deserialize)]
pub struct PullRequestRef {
    pub number: i64,
}

#[derive(Deserialize)]
pub struct IssueRef {
    pub number: i64,
    pub pull_request: Option<PullRequestRef>,
}

#[derive(Deserialize)]
pub struct CheckRunBody {
    pub conclusion: String,
    pub pull_requests: Vec<PullRequestRef>,
    pub check_suite: CheckSuite,
}

#[derive(Deserialize)]
pub struct CheckSuite {
    pub pull_requests: Vec<PullRequestRef>,
}
```

### 7.4 Routing Table (unchanged, giu nguyen tu spec cu)

```
pull_request:
  review_requested → [reviewer]
  labeled/unlabeled  → [fixer]
  opened/reopened/ready_for_review/synchronize → [reviewer, fixer]
  default → ignore

pull_request_review / pull_request_review_comment → [fixer]
check_run (completed, failing conclusion) → [fixer]
push (branch, non-delete) → [fixer as base_branch]
issue_comment → ignore (unconditional)
other → ignore
```

**Failing check_run conclusions**: `FAILURE`, `FAILED`, `ERROR`, `TIMED_OUT`, `ACTION_REQUIRED`.

### 7.5 Locking Strategy

- **Mutex-based**: `sync.Mutex` + `sync.Cond` trong Go.
- Rust tuong duong: `tokio::sync::Mutex` + `tokio::sync::Notify` hoac `std::sync::Mutex` + `std::sync::Condvar`.
- `Forward()` lock toan bo critical section (dedup, enqueue, stats update).
- `worker()` lock dequeue, unlock truoc `executeOnce`, re-lock `finishWork`.

### 7.6 Worker Pool

```rust
// Spawn max_concurrent workers in Tokio
for i in 0..max_concurrent {
    let forwarder = self.clone();
    tokio::spawn(async move {
        forwarder.worker_loop().await;
    });
}
```

**Worker loop**:
```
loop:
  key, item = wait_for_next_work()  // blocks on condvar/notify
  outcome = execute_with_retry(key, item)
  finish_work(key, outcome)
  if closed && queue empty: break
```

### 7.7 Execute with Retry

```rust
async fn execute_with_retry(&self, key: WorkKey, item: WorkItem) -> Outcome {
    let max_retries = 2;
    let mut outcome = Outcome {
        at: self.now().to_rfc3339(),
        project_id: key.project_id,
        repo: key.repo,
        object_type: key.object_type,
        number: key.number,
        lanes: sorted_lanes(&item.lanes),
        event_type: Some(item.metadata.event_type),
        action: Some(item.metadata.action),
        delivery_id: Some(item.metadata.delivery_id),
        ..Default::default()
    };

    for attempt in 1..=max_retries {
        outcome.attempts = attempt;
        match self.execute_once(&key, &item).await {
            Ok(()) => {
                outcome.status = "succeeded".into();
                return outcome;
            }
            Err(e) if attempt < max_retries && is_transient(&e) => {
                self.stats.executions_retried += 1;
                tokio::time::sleep(self.retry_delay).await;
            }
            Err(e) => {
                outcome.status = "failed".into();
                outcome.error = Some(e.to_string());
                return outcome;
            }
        }
    }
    outcome.status = "failed".into();
    outcome.error = Some("targeted discovery exhausted retries".into());
    outcome
}
```

### 7.8 Transient Error Detection

```rust
fn is_transient(err: &Error) -> bool {
    if err.is::<TimeoutError>() || err.is::<DeadlineExceeded>() {
        return true;
    }
    let msg = err.to_string().to_lowercase();
    msg.contains("timeout")
        || msg.contains("tempor")
        || msg.contains("rate limit")
        || msg.contains("retry after")
}
```

---

## 8. Helper Utilities

### 8.1 Path Normalization

```rust
fn normalize_path(path: &str) -> String {
    if path.is_empty() { return "/".into(); }
    if path.len() == 1 { return path.into(); }
    path.trim_end_matches('/').to_string()
}
```

### 8.2 Method Assertion

```rust
fn assert_method(method: &Method, allowed: Method, path: &str) -> Result<(), ApiError> {
    if *method == allowed {
        Ok(())
    } else {
        Err(ApiError::method_not_allowed(path))
    }
}
```

Note: Go version nhan `writeError` callback de ghi response truc tiep. Rust version tra ve `Result` de caller xu ly.

### 8.3 Query String Helpers

```rust
fn query_bool(params: &HashMap<String, String>, key: &str) -> bool {
    params.get(key).map_or(false, |v| {
        v == "1" || v.eq_ignore_ascii_case("true")
    })
}

fn parse_positive_int64(value: &str, field_name: &str) -> Result<i64, ApiError> {
    let parsed: i64 = value.trim().parse().map_err(|_| {
        ApiError::validation_failed(format!("{} must be a positive integer", field_name))
    })?;
    if parsed <= 0 {
        return Err(ApiError::validation_failed(format!("{} must be a positive integer", field_name)));
    }
    Ok(parsed)
}
```

### 8.4 Optional String/Metadata Helpers

```rust
fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim().to_string();
        if trimmed.is_empty() { None } else { Some(trimmed) }
    })
}

fn deref_string(value: &Option<String>) -> String {
    value.as_deref().unwrap_or("").to_string()
}

fn string_ptr_or_nil(value: &str) -> Option<String> {
    let trimmed = value.trim().to_string();
    if trimmed.is_empty() { None } else { Some(trimmed) }
}

fn first_non_empty_string(values: &[Option<String>]) -> Option<String> {
    values.iter().find_map(|v| v.as_ref().filter(|s| !s.trim().is_empty()).cloned())
}

fn parse_json_object(raw: &Option<String>) -> serde_json::Value {
    raw.as_ref().and_then(|s| {
        if s.trim().is_empty() { None }
        else { serde_json::from_str(s).ok() }
    }).unwrap_or(serde_json::Value::Object(Map::new()))
}

// Worklog parsing: read from log files, path traversal protection
// max_persisted_agent_log_read_bytes = 16 MB

fn read_agent_output_log(log_dir: &str, path: &Option<String>) -> (String, String) {
    // Parse outputJSON, resolve stdoutLogPath/stderrLogPath
    // Validate paths with is_path_within_directory
    // Read last 16MB of file
}
```

### 8.5 Path Traversal Protection

```rust
fn is_path_within_directory(path: &str, directory: &str) -> bool {
    if directory.trim().is_empty() { return false; }
    let abs_path = std::fs::canonicalize(path).ok()?;
    let abs_dir = std::fs::canonicalize(directory).ok()?;
    if abs_path == abs_dir { return false; }
    abs_path.starts_with(abs_dir.join(""))
}
```

### 8.6 Project ID Normalization

```rust
fn derive_project_id_from_repo_path(repo_path: &str) -> String {
    let segments: Vec<&str> = repo_path.split(&['/', '\\'][..]).collect();
    let last = segments.last().copied().unwrap_or("project");
    let normalized: String = last.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let normalized = normalized.trim_matches('-').to_string();
    if normalized.is_empty() { "project".into() } else { normalized }
}
```

---

## 9. Config Constants

```rust
pub const API_BASE_PATH: &str = "/api/v1";
pub const WEBHOOK_FORWARD_PATH: &str = "/webhook/forward";
pub const JAVASCRIPT_ISO_STRING_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";
pub const LOOP_LOGS_FOLLOW_POLL_INTERVAL: Duration = Duration::from_millis(200);
pub const ACTIVE_RUN_HEARTBEAT_TTL: Duration = Duration::from_secs(30 * 60);  // 30 minutes
pub const MAX_PERSISTED_AGENT_LOG_READ_BYTES: u64 = 16 * 1024 * 1024;  // 16 MB

// Webhook forwarder defaults
pub const DEFAULT_QUEUE_CAPACITY: usize = 128;
pub const DEFAULT_MAX_CONCURRENT: usize = 4;
pub const DEFAULT_DELIVERY_TTL: Duration = Duration::from_secs(3600);  // 1 hour
pub const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(2);
pub const DEFAULT_RECENT_OUTCOME_SIZE: usize = 64;
pub const MAX_RETRIES: i32 = 2;
```

---

## 10. Serialization Helpers

### 10.1 Loop Serialization

```rust
fn serialize_loop(loop_record: LoopRecord) -> LoopResponse {
    LoopResponse {
        id: loop_record.id,
        seq: loop_record.seq,
        project_id: loop_record.project_id,
        loop_type: loop_record.r#type,
        target_type: loop_record.target_type,
        target_id: loop_record.target_id,
        repo: loop_record.repo,
        pr_number: loop_record.pr_number,
        status: loop_record.status,
        config_json: loop_record.config_json,
        metadata_json: loop_record.metadata_json,
        last_run_at: loop_record.last_run_at,
        next_run_at: loop_record.next_run_at,
        created_at: loop_record.created_at,
        updated_at: loop_record.updated_at,
    }
}
```

### 10.2 Run Serialization

```rust
fn serialize_run(run_record: RunRecord) -> RunResponse {
    // Map fields directly
}
```

### 10.3 Event Serialization

```rust
fn serialize_event(event: EventLogRecord) -> EventResponse {
    EventResponse {
        id: event.id,
        event_type: event.event_type,
        project_id: event.project_id,
        loop_id: event.loop_id,
        run_id: event.run_id,
        entity_type: event.entity_type,
        entity_id: event.entity_id,
        correlation_id: event.correlation_id,
        causation_id: event.causation_id,
        actor_type: event.actor_type,
        actor_id: event.actor_id,
        actor_display_name: event.actor_display_name,
        payload_json: event.payload_json.clone(),
        created_at: event.created_at,
        payload: parse_payload_json(&event.payload_json),
    }
}
```

### 10.4 Project Serialization

```rust
fn serialize_project(project: ProjectRecord, default_base_branch: &str) -> ProjectResponse {
    let metadata = parse_json_object(&project.metadata_json);
    let base_branch = project.base_branch
        .filter(|b| !b.trim().is_empty())
        .unwrap_or_else(|| default_base_branch.to_string());

    ProjectResponse {
        id: project.id,
        name: project.name,
        repo_path: project.repo_path,
        base_branch,
        archived: project.archived,
        repo: string_metadata_ptr(&metadata, "repo"),
        worktree_root: string_metadata_ptr(&metadata, "worktreeRoot"),
        created_at: project.created_at,
        updated_at: project.updated_at,
    }
}
```

---

## 11. Loop Lifecycle Management (mutate/retry)

### 11.1 mutateLoopStatus

```rust
async fn mutate_loop_status(&self, ctx: Context, loop_id: &str, new_status: LoopStatus) -> Result<LoopResponse, ApiError>;
```

**Validation flow**:
1. Get loop by ID → 404 LOOP_NOT_FOUND neu missing.
2. Neu `new_status == Running`:
   - Kiem tra project active (neu co project_id).
   - Kiem tra coding agent configured cho reviewer/fixer/worker/planner loops.
   - Kiem tra reviewer loop khong phai terminal status.
   - Kiem tra unique active loop constraint.
3. Neu `new_status == Running`: set `next_run_at = now`.
4. Neu `new_status == Paused`: set `next_run_at = nil`, cancel queue items.
5. Upsert loop record.
6. Neu `Running`: requeue cancelled queue items (create new queue item neu can).
7. Trigger scheduler tick.

### 11.2 retryLoop

```rust
async fn retry_loop(&self, ctx: Context, req: &HttpRequest, loop_id: &str) -> Result<RetryLoopResponse, ApiError>;

#[derive(Deserialize)]
pub struct RetryLoopRequest {
    pub mode: Option<String>,              // default "auto" (only mode supported)
    pub reset_attempts: Option<bool>,      // default true
}

#[derive(Serialize)]
pub struct RetryLoopResponse {
    pub loop: LoopResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_item_id: Option<String>,
    pub mode: String,
    pub reset_attempts: bool,
}
```

**Validation flow**:
1. Loop found? 404.
2. Project active? 404.
3. Terminal loop status (stopped/terminated/completed)? 400.
4. Terminal reviewer metadata status? 400.
5. Agent configured? 400.
6. Any running runs for this loop? 409.
7. Any active queue item? 409.
8. Unique active loop constraint? 409.
9. Create new queue item with reset or copy from latest.

### 11.3 Loop Target Key (uniqueness)

```rust
fn loop_target_key(target: &LoopTarget) -> String {
    match target.target_type {
        LoopTargetType::Project => format!("project:{}", normalize_project_target_id(&target.project_id)),
        LoopTargetType::Issue => format!("issue:{}:{}", target.repo, target.issue_number),
        LoopTargetType::PullRequest => format!("pull_request:{}:{}", target.repo, target.pr_number),
    }
}
```

### 11.4 Loop Selector Resolution

```rust
async fn resolve_loop(&self, ctx: Context, selector: &str) -> Result<LoopRecord, ApiError>;
// Try parse as seq (i64) -> GetBySeq
// Fallback: GetByID
// 400 if empty, 404 if not found
```

---

## 12. Metrics & Observability

### 12.1 Webhook Forwarder Stats

```json
{
  "deliveriesReceived": 1000,
  "deliveriesDeduped": 200,
  "deliveriesIgnored": 300,
  "deliveriesAccepted": 500,
  "queueCapacity": 128,
  "queueEnqueued": 400,
  "queueCoalesced": 100,
  "queueRejected": 10,
  "executionsStarted": 350,
  "executionsRetried": 20,
  "executionsSucceeded": 330,
  "executionsFailed": 20,
  "inFlight": 2,
  "queued": 10,
  "knownCoalescedKeys": 5,
  "recentOutcomes": [
    {
      "at": "2025-01-01T00:00:00.000Z",
      "projectId": "my-project",
      "repo": "owner/repo",
      "objectType": "pull_request",
      "number": 42,
      "lanes": ["reviewer", "fixer"],
      "status": "succeeded",
      "attempts": 1,
      "error": "",
      "eventType": "pull_request",
      "action": "opened",
      "deliveryId": "abc-123"
    }
  ]
}
```

### 12.2 Webhook Forwarder Lifecycle

1. **New**: create with Options (repos, config, reviewer, fixer, logger, capacities).
2. **Start**: spawn max_concurrent worker goroutines.
3. **Forward**: receive delivery, route, dedup, enqueue.
4. **Worker**: dequeue → execute lanes with retry → record outcome.
5. **Close**: set closed flag, broadcast cond, wait for workers to finish.
6. **Stats**: snapshot under lock.

---

## 13. Testing Considerations

### 13.1 Time Mocking

- Handler nhan `now: Box<dyn Fn() -> DateTime<Utc>>` de override trong tests.
- Forwarder tuong tu nhan `now` function.

### 13.2 Recovery Summary Mocking

- Neu `context.recovery_summary` nil hoac runtime ko implement `RecoverySummary()`, tra ve empty object `{}`.

### 13.3 Webhook Forwarder Mocking

- `Forwarder` trait vs `TargetedReviewer`/`TargetedFixer` interfaces de mock trong unit tests.

### 13.4 RuntimeState Mocking

- Cac method optional (`webhook_forwarder()`, `webhook_status()`, etc.) co default implementation tra `None`, de mock chi can override methods can thiet.
