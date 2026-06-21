# Module: Rate Limiting & Graceful Degradation — Rust Port Spec

> Source files: `internal/config/types.go` (933 lines), `internal/config/defaults.go` (312 lines), `internal/infra/github/errors.go` (177 lines), `internal/reviewer/runner.go` (~6200 lines) `backoffDelay`/`retryDelay`, `internal/loops/failureclass/failureclass.go`, `internal/runtime/scheduler.go` (1757 lines), `internal/webhookforward/forwarder.go`, `internal/infra/notify/gateway.go`, `internal/infra/git/gateway.go` fetch lock retry, `internal/coordinator/runner.go` `shouldRunTick`

---

## 1. Hien trang Go: Kien truc Rate Limiting & Retry

### 1.1 Failure Classification — 4 loai FailureKind

Tat ca cac runner deu dinh nghia `QueueFailureKind` giong nhau:

```go
type QueueFailureKind string

const (
    FailureRetryableTransient   QueueFailureKind = "retryable_transient"   // Co the retry, exponential backoff
    FailureRetryableAfterResume QueueFailureKind = "retryable_after_resume" // Can resume sau khi restart
    FailureNonRetryable         QueueFailureKind = "non_retryable"          // Permanent failure, khong retry
    FailureManualIntervention   QueueFailureKind = "manual_intervention"   // Can human action
)
```

**No circuit breaker trong Go codebase hien tai.** Khong co concept "open circuit" hay "half-open" state cho bat ky API hay service nao. Moi scheduler tick deu co gang goi GitHub API, neu that bai thi classify failure roi retry queue item, nhung khong co co che de "ngung goi API trong N giay sau N lan that bai lien tiep."

### 1.2 Retry-Max Per Runner

Gia tri `RetryMaxAttempts` toan cuc configurable, default = -1 (infinite). Config schema:

```go
// internal/config/types.go
RetryMaxAttempts  int   // default -1 (infinite), >=1 = limited
RetryBaseDelayMS  int   // default 5000 (5s)
```

Cac runner co retry policy rieng:

- **Planner**: `RetryMaxAttempts = 3`
- **Reviewer**: `RetryMaxAttempts = 5`
- **Fixer**: `RetryMaxAttempts = 3`
- **Worker**: `RetryMaxAttempts = 3`

Ngoai ra, `roles.reviewer.behavior.retry.maxDelayMs` co the override maxDelay cho reviewer rieng. Default cho cac runner khac la `maxRetryDelay = 300 * time.Second`.

### 1.3 Exponential Backoff

```go
// Reviewer implementation (others similar)
func backoffDelay(base time.Duration, attempts int64, maxDelay time.Duration) time.Duration {
    delay := base
    for i := int64(1); i < attempts; i++ {
        if delay >= maxDelay || delay > maxDelay/2 {
            return maxDelay
        }
        delay *= 2
    }
    if delay > maxDelay { return maxDelay }
    return delay
}
```

- `baseDelay = 5s` (default `RetryBaseDelayMS`)
- Cong thuc: `base * 2^(attempt - 1)`, cap o `maxDelay = 300s`
- Jitter: `delay += random(0, delay/4)` de tranh thundering herd

```go
func jitterDelay(delay time.Duration, maxDelay time.Duration) time.Duration {
    maxJitter := delay / 4   // max 25% jitter
    n, _ := rand.Int(rand.Reader, big.NewInt(int64(maxJitter)+1))
    delay += time.Duration(n.Int64())
    if delay > maxDelay { return maxDelay }
    return delay
}
```

### 1.4 Retry-After Headers

Neu GitHub tra ve `retry-after` header, delay duoc tinh tu header do thay vi exponential backoff:

```go
var retryAfterPattern = regexp.MustCompile(`(?i)retry-after\s*[:=]\s*(\d+)`)

func retryAfterDelay(err error) (time.Duration, bool) {
    matches := retryAfterPattern.FindStringSubmatch(err.Error())
    if len(matches) != 2 { return 0, false }
    seconds, _ := time.ParseDuration(matches[1] + "s")
    return seconds, true
}
```

Tham khao: reviewer `run.go` lines 356-361 (`retryDelay`), `backoffDelay` line 5935.

### 1.5 Transient Detection — pattern matching

**Github transients** (`internal/infra/github/errors.go`):

```go
func isTransientGitHubMessage(message string) bool {
    for _, fragment := range []string{
        "tls handshake timeout", "unexpected eof", "connection reset by peer",
        "connection refused", "connection timed out", "i/o timeout",
        "temporary failure in name resolution", "no such host",
        "network is unreachable", "stream error",
        "http2: server sent goaway",
        "http 502", "502 bad gateway",
        "http 503", "503 service unavailable",
        "http 504", "504 gateway timeout",
        "secondary rate limit", "rate limit exceeded",
        "api rate limit exceeded", "graphql: something went wrong",
    } {
        if strings.Contains(message, fragment) { return true }
    }
    return false
}
```

`IsTransientError` duoc goi tu tat ca cac runner:
- `reviewer.runner.go` (5 call sites)
- `fixer.runner.go` (1 call site)
- `planner.runner.go` (1 call site)
- `worker.runner.go` (1 call site)
- `coordinator.runner.go` (1 call site)
- `runtime/runtime.go` (1 call site)

**Enhanced transient classification** (reviewer only, off by default):
Gom them pattern tu `internal/config/reviewer_retry.go`:

```go
var enhancedReviewerTransientPatterns = []string{
    "tls handshake timeout", "unexpected eof", "connection reset",
    "connection refused", "connection timed out", "i/o timeout",
    "no such host", "network is unreachable", "broken pipe",
    "http 5", "502", "503", "504",
}
```

### 1.6 Boundary-Aware Failure Classification (`internal/loops/failureclass/`)

Da co implementation trong Go voi `Boundary` type:

```go
type Boundary string
const (
    BoundaryGitRemote     Boundary = "git_remote"       // External → RetryableTransient
    BoundaryGitLocal      Boundary = "git_local"        // Internal → NonRetryable
    BoundaryGitHubAPI     Boundary = "github_api"       // External → RetryableTransient (tru 400/422)
    BoundaryModelProvider Boundary = "model_provider"   // External → RetryableTransient
    BoundaryAgentProcess  Boundary = "agent_process"    // External → RetryableTransient
    BoundaryLocalWorktree Boundary = "local_worktree"   // ManualIntervention (dirty)
    BoundaryStorage       Boundary = "storage"          // Internal → NonRetryable
    BoundaryConfig        Boundary = "config"            // Internal → NonRetryable
    BoundaryCheckpoint    Boundary = "checkpoint"        // Internal → NonRetryable
    BoundaryPolicy        Boundary = "policy"            // Internal → NonRetryable
    BoundaryUnknown       Boundary = "unknown"           // Fallback → NonRetryable
)
```

`Classify(err, Context) -> Kind`:

1. `errors.Is(err, context.Canceled/DeadlineExceeded)` → `RetryableTransient`
2. `githubinfra.IsTransientError(err)` → `RetryableTransient`
3. Theo `ctx.Boundary`:
   - External boundaries: `RetryableTransient`
   - Internal deterministic: `NonRetryable`
   - `local_worktree`: `ManualIntervention`
   - `unknown`: `NonRetryable`
4. Fallback: `NonRetryable`

### 1.7 Scheduler Overload Protection

**Max concurrent runs**:
- Default: `Scheduler.MaxConcurrentRuns = 3`
- Cach tinh: `availableSlots = maxConcurrentRuns - repos.Queue.CountByStatus("running")`
- Neu `availableSlots == 0`: goi stale-run reconciliation truoc, recompute
- Neu van `== 0`: items khong duoc claim

**Slow lane** (`internal/storage/queue_priorities.go`):
- Threshold: `QueueLongTermRetryAttemptThreshold = 5`
- Predicate: `qi.attempts >= 5 AND last_error_kind IN ('retryable_transient', 'retryable_after_resume', 'non_retryable')`
- Two claim queries:
  1. `ClaimNextNonLongTermRetry()` — normal priority items
  2. `ClaimNextLongTermRetry()` — items da >= 5 attempts, chi claim khi con slot sau khi normal items da claim
- Ordering trong claim: `ORDER BY CASE WHEN longTermRetry THEN 1 ELSE 0 END ASC, priority DESC, created_at ASC`

**Per-project rate limit cho coordinator**:
- `shouldRunTick(projectID)`: kiem tra `LastRunAt` vs `MinInterval`
- Default `MinInterval`: configurable per project

### 1.8 Webhook Forwarder

- `QueueCapacity = 128` (default)
- `MaxConcurrentWorkers = 4` (default)
- `DeliveryTTL = 1 hour`
- `maxRetries = 2`
- `defaultRetryDelay = 2s`
- In-flight tracking: `currentInFlight` field
- `Forwarder.Forward()`: kiem tra backpressure (InFlight >= QueueCapacity → reject)

### 1.9 Git Fetch Lock Retry

`internal/infra/git/gateway.go`:
- `fetchRefLockRetryDelays = [50 * time.Millisecond, 100 * time.Millisecond, 250 * time.Millisecond]`
- Toi da 3 attempts (retry 2 lan)
- Neu van that bai sau 3 attempts → `RetryableTransient`

### 1.10 Notification Throttle

`internal/infra/notify/gateway.go`:
- `ThrottleWindowSeconds`: default 60s
- Moi dedupe key chi gui duoc 1 notification trong 60s
- Keys: `"loop_{loopID}_failure"`, `"agent_{executionID}_timeout"`, etc.
- In-memory map `lastSentAt`, auto-clean khi runtime stop

### 1.11 Daemon Restart Throttle

- `Daemon.RestartThrottleSeconds`: default 10 (configurable)
- Duoc su dung trong `launchd` plist `ThrottleInterval`
- Dam bao looperd khong restart spam khi crash lien tuc

### 1.12 Agent Timeout (Per Runner)

| Runner | MaxRuntime | IdleTimeout |
|--------|-----------|-------------|
| Planner | 3600s (1h) | 600s (10m) |
| Worker | 10800s (3h) | 900s (15m) |
| Reviewer | 5400s (90m) | 600s (10m) |
| Fixer | 7200s (2h) | 600s (10m) |

Idle timeout: neu khong co output trong `idleTimeout` → SIGTERM → 5s grace → SIGKILL

### 1.13 Reviewer Loop Timings

- `QuietPeriodSeconds`: default 60s
- `MinPublishIntervalSeconds`: default 300s
- Reviewer khong publish review trong quiet period sau PR change
- `MinPublishInterval` gioi han tan suat publish review

### 1.14 Agent Native Resume

`internal/runtime/shell.go`:
- `resolveNativeResume`: kiem tra co the resume tu agent execution truoc
- Agent vendors ho tro:
  - Claude Code: `--resume`
  - Codex: `resume`
  - OpenCode: `--session`
  - Cursor CLI: `--resume`
- Neu native resume that bai (stderr chua "resume failed") → fallback checkpoint restart
- `NativeResumeStatus`: "pending" → "started" → "failed"/"completed" → "fallback_started" → "fallback_completed"

---

## 2. Rust Port Design

### 2.1 RetryPolicy (shared type)

```rust
/// RetryPolicy defines exponential backoff parameters.
/// Duoc dung boi tat ca cac runner va service.
pub struct RetryPolicy {
    /// So lan retry toi da. -1 = infinite, >=1 = limit.
    pub max_attempts: i32,
    /// Base delay cho exponential backoff. Default 5s.
    pub base_delay: Duration,
    /// Max delay cap. Default 300s (5 phut).
    pub max_delay: Duration,
    /// Exponential multiplier. Default 2.0.
    pub multiplier: f64,
    /// Jitter fraction (0.0 - 1.0). Default 0.1 (10%).
    pub jitter: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: -1,
            base_delay: Duration::from_secs(5),
            max_delay: Duration::from_secs(300),
            multiplier: 2.0,
            jitter: 0.1,
        }
    }
}

impl RetryPolicy {
    /// Tinh delay cho attempt thu `n` (1-based).
    /// delay = min(max_delay, base_delay * multiplier^(n-1))
    /// Them jitter: +- random(0, delay * jitter)
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        if self.max_delay <= Duration::ZERO {
            return Duration::ZERO;
        }
        let mut delay = self.base_delay.as_secs_f64();
        for _ in 1..attempt {
            delay *= self.multiplier;
            if delay >= self.max_delay.as_secs_f64() {
                return self.max_delay;
            }
        }
        let delay = delay.min(self.max_delay.as_secs_f64());
        if delay <= 0.0 {
            return Duration::ZERO;
        }
        // Jitter: +/- jitter%
        let jitter_amount = delay * self.jitter;
        let offset = rand::thread_rng().gen_range(-jitter_amount..=jitter_amount);
        let final_delay = (delay + offset).max(0.0);
        Duration::from_secs_f64(final_delay).min(self.max_delay)
    }

    /// Kiem tra xem attempt hien tai da vuot qua max_attempts chua.
    pub fn is_exhausted(&self, attempt: u32) -> bool {
        if self.max_attempts < 0 {
            return false; // infinite
        }
        attempt >= self.max_attempts as u32
    }

    /// Tao delay dua tren `retry-after` header (neu co).
    /// Neu khong co, dung exponential backoff.
    pub fn delay_with_retry_after(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(ra) = retry_after {
            return ra.min(self.max_delay);
        }
        self.delay_for_attempt(attempt)
    }
}
```

**Khong gian tham so**:
- `base_delay`: tu config `scheduler.retryBaseDelayMs` (default 5000ms)
- `max_delay`: tu config `roles.reviewer.behavior.retry.maxDelayMs` hoac default 300s
- `multiplier`: luon la 2.0 (exponential)
- `jitter`: 0.1 (10%) — Go dung 25% (delay/4), nhung 10% du but de tranh thundering herd
- `max_attempts`: tu `scheduler.retryMaxAttempts` hoac per-runner override

### 2.2 Failure Kind Enum (shared)

```rust
/// 4 loai failure kind giong Go, dung boi moi runner va QueueItem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// Temporary error: network, API timeout, service unavailable, etc.
    /// Co the retry tu dong voi exponential backoff.
    RetryableTransient,
    /// Can resume sau khi restart. Co checkpoint valid.
    /// Retry voi resume context.
    RetryableAfterResume,
    /// Permanent failure. Khong retry tu dong.
    /// VD: config validation, schema error, storage failure.
    NonRetryable,
    /// Can human intervention. Khong retry tu dong.
    /// VD: dirty worktree, auto-push disabled, policy denial.
    ManualIntervention,
}

impl FailureKind {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::RetryableTransient | Self::RetryableAfterResume)
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::NonRetryable | Self::ManualIntervention)
    }
}
```

### 2.3 Failure Boundary (shared classifier)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
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

impl Boundary {
    /// Xac dinh classification mac dinh cho boundary nay.
    pub fn default_classification(&self) -> FailureKind {
        match self {
            // External → retryable transient
            Self::GitRemote | Self::GitHubApi
            | Self::ModelProvider | Self::AgentProcess
            => FailureKind::RetryableTransient,
            // Local deterministic → non-retryable
            Self::GitLocal | Self::Storage
            | Self::Config | Self::Checkpoint | Self::Policy
            => FailureKind::NonRetryable,
            // Worktree issue → manual intervention
            Self::LocalWorktree => FailureKind::ManualIntervention,
            // Unknown → non-retryable (conservative)
            Self::Unknown => FailureKind::NonRetryable,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClassificationContext {
    pub runner: RunnerKind,
    pub step: String,
    pub boundary: Boundary,
    pub side_effect_state: SideEffectState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideEffectState {
    /// Chua co side effect nao
    None,
    /// Da co side effect (tx commit, PR comment, etc.)
    PrePublish,
    /// Side effect da xay ra nhung chua xac dinh trang thai
    PostPublishAmbiguous,
}

pub fn classify_failure(err: &dyn Error, ctx: &ClassificationContext) -> FailureKind {
    // 1. Context.Canceled / DeadlineExceeded → RetryableTransient
    if err.downcast_ref::<Cancelled>().is_some()
        || err.downcast_ref::<TimeoutError>().is_some()
    {
        return FailureKind::RetryableTransient;
    }

    // 2. GitHub transient pattern matching
    if is_github_transient_error(err) {
        return FailureKind::RetryableTransient;
    }

    // 3. Boundary-aware classification
    match ctx.boundary {
        Boundary::GitHubApi => {
            // GitHub API co ngoai le: deterministic HTTP errors
            if is_github_deterministic_denial(err) {
                return FailureKind::NonRetryable;
            }
            // External boundary: co gang parse retry-after header
            if let Some(retry_after) = parse_retry_after(err) {
                // RetryAfter da duoc xu ly o level retry policy
            }
            FailureKind::RetryableTransient
        }
        Boundary::LocalWorktree => {
            FailureKind::ManualIntervention
        }
        other => other.default_classification(),
    }
}

/// Kiem tra HTTP denial deterministic: 400, 401, 403, 404, 422
fn is_github_deterministic_denial(err: &dyn Error) -> bool {
    let msg = err.to_string().to_lowercase();
    for pat in &["http 400", "http 401", "http 403", "http 404",
                 "http 422", "http 400", "401", "403", "404",
                 "resource not accessible", "could not resolve",
                 "not found", "bad credentials"] {
        if msg.contains(pat) { return true; }
    }
    false
}

/// Parse `retry-after: N` tu error message
fn parse_retry_after(err: &dyn Error) -> Option<Duration> {
    let re = regex::Regex::new(r"(?i)retry-after\s*[:=]\s*(\d+)").ok()?;
    let caps = re.captures(&err.to_string())?;
    let secs: u64 = caps.get(1)?.as_str().parse().ok()?;
    Some(Duration::from_secs(secs))
}
```

### 2.4 Transient Error Pattern Matching (Rust)

```rust
/// Giong Go `isTransientGitHubMessage` + enhanced patterns
const TRANSIENT_PATTERNS: &[&str] = &[
    "tls handshake timeout",
    "unexpected eof",
    "connection reset by peer",
    "connection refused",
    "connection timed out",
    "i/o timeout",
    "temporary failure in name resolution",
    "no such host",
    "network is unreachable",
    "stream error",
    "http2: server sent goaway",
    "http 502",
    "502 bad gateway",
    "http 503",
    "503 service unavailable",
    "http 504",
    "504 gateway timeout",
    "secondary rate limit",
    "rate limit exceeded",
    "api rate limit exceeded",
    "graphql: something went wrong",
    "http 429",
    "429 too many requests",
    // Enhanced transient patterns (optional, off by default)
    "broken pipe",
    "http 5",  // catch 500, 501, etc.
];

/// Enhanced patterns — giong Go `enhancedTransientClassification`
const ENHANCED_TRANSIENT_PATTERNS: &[&str] = &[
    "tls handshake timeout",
    "unexpected eof",
    "connection reset",
    "connection refused",
    "connection timed out",
    "i/o timeout",
    "no such host",
    "network is unreachable",
    "broken pipe",
    "http 5",
    "502",
    "503",
    "504",
];

pub fn is_github_transient_error(err: &dyn Error) -> bool {
    let msg = err.to_string().to_lowercase();
    // Check "looks like GitHub failure"
    let is_github = [
        "github", "api.github.com", "graphql",
        "gh api", "gh pr",
    ].iter().any(|s| msg.contains(s));

    if !is_github {
        return false;
    }

    TRANSIENT_PATTERNS.iter().any(|p| msg.contains(p))
}
```

### 2.5 Scheduled Overload Protection

```rust
pub struct SchedulerOverloadGuard {
    pub max_concurrent_runs: u32,    // default: 3
    pub slow_lane_threshold: u32,    // default: 5 attempts
}

impl SchedulerOverloadGuard {
    /// Tinh available slots.
    /// runningCount duoc lay tu DB: COUNT queue_items WHERE status = 'running'
    pub fn available_slots(&self, running_count: u32) -> u32 {
        if running_count >= self.max_concurrent_runs {
            return 0;
        }
        self.max_concurrent_runs - running_count
    }

    /// Kiem tra xem item co thuoc slow lane khong.
    pub fn is_slow_lane(&self, attempts: u32) -> bool {
        attempts >= self.slow_lane_threshold
    }

    /// Slow lane claim priority: chi claim khi con slot sau normal lane.
    pub fn should_claim_slow_lane(&self, slots: u32, normal_claimed: u32) -> bool {
        normal_claimed < self.max_concurrent_runs && slots > 0
    }
}
```

**Priority ordering trong claim phase** (giong Go):
1. `ClaimNextNonLongTermRetry()` — normal priority items
2. `ClaimNextLongTermRetry()` — items >= 5 attempts

**Priority per runner type** (giong Go):
```
planner > reviewer > fixer > worker
```

**Stale run reconciliation khi available_slots == 0**:
```
1. Kiem tra running runs co stale heartbeat (30 phut)
2. Neu stale: interrupt run, cleanup executions
3. Requeue queue item neu can
4. Recompute available_slots
```

### 2.6 GitHub Rate Limit Handling

```rust
/// Detects rate limit tu gh CLI output.
pub fn detect_rate_limit(stderr: &str) -> Option<RateLimitInfo> {
    let stderr_lower = stderr.to_lowercase();

    // Check for retry-after header
    let re = regex::Regex::new(r"(?i)retry-after\s*[:=]\s*(\d+)").ok()?;
    let retry_after_secs = re.captures(stderr)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<u64>().ok());

    // Check for primary rate limit
    if stderr_lower.contains("rate limit exceeded")
        || stderr_lower.contains("api rate limit exceeded")
        || stderr_lower.contains("http 429")
        || stderr_lower.contains("429 too many requests")
    {
        return Some(RateLimitInfo {
            kind: RateLimitKind::Primary,
            retry_after: retry_after_secs.map(Duration::from_secs),
        });
    }

    // Check for secondary rate limit
    if stderr_lower.contains("secondary rate limit") {
        return Some(RateLimitInfo {
            kind: RateLimitKind::Secondary,
            retry_after: retry_after_secs
                .map(Duration::from_secs)
                .or(Some(Duration::from_secs(60))), // default 60s cho secondary
        });
    }

    None
}

#[derive(Debug, Clone)]
pub struct RateLimitInfo {
    pub kind: RateLimitKind,
    pub retry_after: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitKind {
    /// Primary rate limit: 5000 requests/hour.
    /// Retry-after header thuong la vai giay den vai phut.
    Primary,
    /// Secondary rate limit: abuse detection, compute-intensive queries.
    /// Retry-after header thuong la vai phut den 1 gio.
    Secondary,
}
```

**How rate limit flows through the system**:

```
gh command fails with 429
  └─> detect_rate_limit(stderr) → Some(RateLimitInfo)
       └─> classify_failure() → RetryableTransient
            └─> RetryPolicy.delay_with_retry_after(attempt, retry_after)
                 ├─ Neu retry_after present → dung retry_after value
                 └─> Neu khong co retry_after → exponential backoff default
                      └─> Queue item duoc set retry_at = now + delay
                           └─> Scheduler claim skip items chua toi han retry
```

**Queue item state khi bi rate-limit**:
```json
{
  "status": "queued",
  "failure_kind": "retryable_transient",
  "last_error": "GitHub API rate limit exceeded (retry after 3600s)",
  "retry_at": "2026-06-21T12:00:00.000Z",
  "attempts": 3
}
```

### 2.7 Graceful Degradation Scenarios

#### 2.7.1 GitHub API DOWN

**Symptoms**:
- Tat ca `gh api`, `gh pr`, `gh issue` calls deu fail
- Scheduler tick that bai o discovery phase
- `IsTransientError` tra ve `true` cho moi error

**What happens in Rust**:
```
1. Scheduler tick starts
2. Discovery phase: github_api calls fail → classify = RetryableTransient
3. Queue items remain queued (khong co new items duoc tao)
4. Running agents continue (they have their own gh binary / session)
5. Items duoc retry voi exponential backoff
6. Neu max_attempts exhausted → manual_intervention

Per-project impact:
- Planner: cannot discover issues → no new planning
- Reviewer: cannot discover PRs → no new reviews
- Worker: cannot discover issues → no new work
- Fixer: cannot discover PRs → no new fixes
- Coordinator: cannot triage, cannot dispatch → mergewatch stalls
```

**Circuit breaker (new for Rust) — prevents cascade failures**:

```rust
pub struct GitHubCircuitBreaker {
    /// Gan cua hien tai: Closed, Open, HalfOpen
    state: AtomicState,
    /// So lan that bai lien tiep de mo circuit
    failure_threshold: u32,       // default: 5
    /// Thoi gian cho truoc khi chuyen tu Open → HalfOpen
    cooldown: Duration,           // default: 60s
    /// Thoi gian cho truoc khi HalfOpen → Open lai
    half_open_timeout: Duration,  // default: 10s

    // Per-operation counters
    discovery_failures: AtomicU32,
    mutation_failures: AtomicU32,
    last_failure_at: AtomicInstant,
    last_success_at: AtomicInstant,
}

enum CircuitState {
    Closed,   // Normal operation
    Open,     // Reject all requests immediately
    HalfOpen, // Allow probe request
}

enum OperationKind {
    Discovery,  // Read operations: list PRs, list issues, get PR detail
    Mutation,   // Write operations: post comment, add label, merge PR
}

impl GitHubCircuitBreaker {
    /// Kiem tra truoc khi goi GitHub API.
    pub fn allow_request(&self, kind: OperationKind) -> bool {
        match self.state.load() {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // Check cooldown
                if self.last_failure_at.elapsed() >= self.cooldown {
                    self.state.store(CircuitState::HalfOpen);
                    return true; // allow probe
                }
                false
            }
            CircuitState::HalfOpen => {
                // Chi cho phep 1 request
                true
            }
        }
    }

    /// Goi sau moi lan GitHub API call thanh cong.
    pub fn record_success(&self, kind: OperationKind) {
        match kind {
            OperationKind::Discovery => self.discovery_failures.store(0),
            OperationKind::Mutation => self.mutation_failures.store(0),
        }
        self.state.store(CircuitState::Closed);
        self.last_success_at.store(Instant::now());
    }

    /// Goi sau moi lan GitHub API call that bai.
    pub fn record_failure(&self, kind: OperationKind) {
        let counter = match kind {
            OperationKind::Discovery => &self.discovery_failures,
            OperationKind::Mutation => &self.mutation_failures,
        };
        let failures = counter.fetch_add(1) + 1;
        if failures >= self.failure_threshold {
            self.state.store(CircuitState::Open);
        }
        self.last_failure_at.store(Instant::now());
    }
}
```

**Circuit breaker scope**: per-GitHub-Gateway instance (toan bo daemon), khong phai per-project. Co the mo rong thanh per-operation neu can.

**Per-operation circuit breakers**:
- **Discovery circuit**: list PRs/issues, get PR detail — read-only operations
  - Neu discovery bi Open: scheduler tick skip discovery phase
- **Mutation circuit**: post comment, add label, merge — write operations
  - Neu mutation bi Open: reviewer publish, worker push bi block
  - In-app notification van duoc ghi (khong can GitHub)

**Edge case**: circuit breaker + rate limit → double protection
```
Rate limit (429) → classify RetryableTransient
  ├─ Neu circuit breaker da Open: reject ngay, khong goi GitHub
  └─ Neu circuit breaker Closed: goi GitHub, that bai → record_failure
       └─ Neu >= 5 failures lien tiep → circuit Open
            └─ Scheduler tick skip discovery → retry trong 60s
```

#### 2.7.2 Git LOCKED

**Symptoms**:
- `git fetch`, `git push`, `git worktree add` fail vi lock file
- Worktree operations cannot proceed

**What happens in Rust**:
```
1. GitGateway detects lock contention
2. Retry 3 times: 50ms, 100ms, 250ms (giong Go fetchRefLockRetryDelays)
3. Neu van locked → classify = RetryableTransient (git_remote boundary)
4. Runner retry with exponential backoff
5. Persistent lock (> max_attempts) → ManualIntervention

Detailed flow:
  fetch_lock_contention:
    attempt=1 → 50ms → retry → locked
    attempt=2 → 100ms → retry → locked
    attempt=3 → 250ms → retry → STILL LOCKED
    └─> classify = RetryableTransient (git_remote)
         └─> Queue item retry with baseDelay=5s, attempt+=1
              └─> After max_attempts → ManualIntervention
                   └─> Human must kill blocking process

worktree prepare failure:
    git worktree add fails → classify = RetryableTransient (git_remote)
    Queue item retried at next scheduler tick
    After max_attempts → ManualIntervention
```

**Implementation**:

```rust
pub fn fetch_with_lock_retry(
    ctx: &Context,
    fetch_fn: impl Fn() -> Result<(), GitError>,
    max_retries: u32,  // default 3
) -> Result<(), GitError> {
    let delays = [Duration::from_millis(50),
                  Duration::from_millis(100),
                  Duration::from_millis(250)];

    let mut last_error = None;
    for attempt in 0..max_retries {
        match fetch_fn() {
            Ok(()) => return Ok(()),
            Err(e) if is_lock_contention(&e) => {
                last_error = Some(e);
                if attempt < max_retries - 1 {
                    sleep(delays[attempt as usize]);
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_error.unwrap())
}

fn is_lock_contention(err: &GitError) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("unable to acquire lock")
        || msg.contains("existing lock")
        || msg.contains("lock file")
        || msg.contains("is held by")
}
```

#### 2.7.3 osascript UNAVAILABLE

**Symptoms**:
- `osascript` binary not found in PATH
- macOS notification mechanism unavailable

**What happens in Rust**:
```
1. NotificationGateway detect osascript path invalid hoặc execution fail
2. Log warning: "osascript unavailable, notifications disabled"
3. NOT a fatal error: startup continues
4. In-app notification (DB) van duoc ghi → `event_logs` table
5. CLI `looper ps` van hien thi notification history tu DB

Failure scenario: osascript not found
  detect: tools.osascriptPath not set AND osascript not in PATH
  result: OsascriptBackend disabled, log warning
  impact: No macOS desktop notifications
  recovery: User installed osascript, restart looperd

Failure scenario: osascript execution timeout (>35s)
  detect: RunCommand timeout
  result: Log error, skip notification, continue
  impact: Single notification lost (throttle key prevents spam on retry)
```

#### 2.7.4 Agent CRASHES

**Symptoms**:
- Agent subprocess (Claude Code, Codex, etc.) exits unexpectedly
- SIGCHLD received, process exit detected
- Khong co completion marker hoac marker incomplete

**What happens in Rust**:
```
1. Shell execution detects process exit (exit code != 0 or signal)
2. `Result.status` = "failed", "timeout", or "killed"
3. classify_failure → boundary = AgentProcess → RetryableTransient

Recovery path:
  [NativeResume enabled]
  1. Check agent supports native resume (Claude Code --resume, etc.)
  2. Build resume command: `claude --resume <session_id>`
  3. Spawn new agent session
  4. Neu native resume succeed → continue
  5. Neu native resume fail (stderr "resume failed") → fallback to checkpoint restart

  [NativeResume disabled]
  1. Read last checkpoint from DB
  2. Spawn new agent from last checkpoint
  3. Agent runs from scratch (no session context)

  [Both fail]
  1. classify = RetryableTransient (AgentProcess)
  2. Queue item retried at next scheduler tick
  3. New Run created for retry
  4. After max_attempts → ManualIntervention

Cac loai agent failure:
  timeout (max_runtime)  → Result.status = "timeout", TimeoutType = "max_runtime"
  timeout (idle)         → Result.status = "timeout", TimeoutType = "idle"
  killed (SIGTERM/SIGKILL) → Result.status = "killed"
  agent internal error   → Result.status = "failed", ParseStatus = "invalid_json"/"missing"
  agent explicit fail    → Result.status = "failed", có completion marker với status = "failed"

Double timeout protection:
  max_runtime timeout → SIGTERM process group
       └─> 5s grace period → SIGKILL process group
            └─> classify → RetryableTransient (AgentProcess)
                 └─> Runner retry with exponential backoff
                      └─> New session (khong resume vi da het runtime)
```

#### 2.7.5 DISK FULL

**Symptoms**:
- SQLite write fails: `sqlite3_step() → SQLITE_FULL`
- Log write fails: `write() → ENOSPC`
- Git worktree operations fail: `write() → ENOSPC`
- agent output persist fails

**What happens in Rust**:
```
Failure location        Classification          Recovery
─────────────────────────────────────────────────────────────
SQLite write fail       Storage → NonRetryable  ManualIntervention
Log write fail          Log warning, continue   No automated recovery
Worktree operation      GitLocal → NonRetryable ManualIntervention
Agent output persist    Storage → NonRetryable  Fallback to in-memory

Detailed flow:
  [SQLite write fails]
  1. Storage query fails with Os { code: 28, kind: StorageFull, ... }
  2. classify = NonRetryable (Boundary::Storage)
  3. Queue item → ManualIntervention
  4. Scheduler tick fails → looperd cannot operate
  5. Human must free disk space, then retry

  [Log write fails]
  1. Log rotation cannot write new file
  2. Log to stderr: "disk full, cannot write log file"
  3. Continue operation (best-effort)
  4. Agent output still captured in bounded in-memory buffer

  [Worktree operation fails]
  1. Git operation fails with Os { code: 28 }
  2. classify = NonRetryable (Boundary::GitLocal)
  3. Queue item → ManualIntervention
  4. No automated git cleanup (git also fails)

  [Agent output persist fails]
  1. persistStatus() fails on disk write
  2. Fallback: keep agent output in bounded in-memory buffer (256KB)
  3. On agent completion: try to persist again
  4. Neu van fail: verlieren output (log warning)
```

#### 2.7.6 Webhook Subsystem OVERLOAD

**Symptoms**:
- Queue capacity reached (128 items)
- Delivery backlog > 1 hour TTL
- Concurrent workers all busy (4 workers)

**What happens in Rust**:
```
1. Webhook receiver accept incoming webhook
2. Forwarder.Forward() checks:
   - Is queue full? (InFlight >= QueueCapacity)
     YES → return status="rejected", reason="queue_full"
     NO  → enqueue delivery
3. Delivery worker:
   - Has TTL check (1h): neu expired → drop with event "delivery.ttl_expired"
   - Max retries: 2 (if delivery fails)
   - Retry delay: 2s

Backpressure effects:
  Incoming webhooks rejected → GitHub retries webhook delivery (GitHub retries for 24h)
  Looper does NOT crash → webhook receiver continues accepting new connections
  Webhook forwarder stats show queue backlog

Recovery:
  Workers finish current deliveries → pick next from queue
  Queue drains naturally
  Incoming webhooks accepted again when InFlight < QueueCapacity
```

#### 2.7.7 Daemon Crash in a Loop

**Symptoms**:
- looperd crashes immediately after startup
- Launchd/Supervisor restarts it immediately
- Crash loop: start → fail → restart → fail → ...

**What happens in Rust**:
```
1. looperd started
2. Bootstrap phase: fail at LoadConfig, ValidateToolPaths, or EnsureDirs
3. Daemon exits with non-zero exit code
4. Supervisor (launchd/systemd) detects exit → restarts
5. Restart throttle (default 10s) → delay before next restart
6. If crash continues: launchd stops after 5 failures in quick succession

Protection layers:
  [launchd] ThrottleInterval = Daemon.RestartThrottleSeconds = 10s
  [systemd] StartLimitIntervalSec=60, StartLimitBurst=5 (example)
  [supervisor] autorestart=true, startretries=3, startsecs=10

If daemon starts but fails during Runtime.Start:
  Bootstrap succeeded but StartRuntime failed
  → Daemon logs error, exits
  → Supervisor restarts after throttle
  → Neu fail same way: crash loop continues until config fixed
```

#### 2.7.8 Network Partition

**Symptoms**:
- All GitHub API calls fail with timeout/connection refused
- Git operations fail with `host unreachable`
- DNS resolution fails

**What happens in Rust**:
```
1. All external operations fail
2. classify = RetryableTransient (GitHubApi or GitRemote)
3. Running agents continue (they have their own network stack)
4. New work cannot be discovered
5. Scheduler tick fails → skip phase

Flow:
  Scheduler tick starts
    └> Discovery phase: gh api fails (connection refused)
         classify = RetryableTransient (GitHubApi)
         RetryPolicy: next_retry in 5s, 10s, 20s, 40s, 80s, 160s, 300s (capped)
         Circuit breaker: after 5 consecutive failures → Open
           └> Next scheduler tick: circuit breaker says Open → skip discovery immediately
           └> No actual gh call made → no timeout delay
           └> Scheduler completes tick quickly → save CPU

  Network restored:
    After cooldown (60s): circuit HalfOpen
    Scheduler tick: circuit allows 1 probe
    gh api call succeeds → circuit Closed
    Normal operation resumes
```

### 2.8 Fallback Chains

#### Agent Execution Fallback
```
1. Fresh spawn → agent command starts from scratch
   └> Fail: Stdout empty, exit code != 0
2. Native resume → `claude --resume <session_id>`
   └> Fail: "resume failed" in stderr, or agent doesn't support resume
3. Checkpoint restart → spawn new agent with last checkpoint context
   └> Fail: Agent produces no valid output
4. ManualIntervention → queue item status = manual_intervention
   └> Human reviews, fixes, runs `looper retry <seq>`
```

#### PR Creation Fallback (Worker)
```
1. Existing PR adoption → find PR matching branch, reuse it
   └> Fail: No existing PR found, or PR is closed
2. Create new PR via `gh pr create`
   └> Fail: GitHub API error, branch conflict, etc.
3. ManualIntervention → push exists but no PR
   └> Human creates PR manually
```

#### Queue Claim Fallback
```
1. ClaimNextNonLongTermRetry() -> non-long-term retry items first
   └> No items available
2. ClaimNextLongTermRetry() -> items with >=5 attempts
   └> No items available
3. No-op -> tick completes immediately, wait for next poll
```

#### Review Finding Fallback
```
1. Agent review → Claude Code reviewers PR
   └> Agent crash, timeout, or produces invalid output
2. Criteria-based auto-eval → simple regex/pattern checks on diff
   └> Not applicable (PR is complex, requires understanding)
3. Manual review → PR stays open, user notified via `looper ps`
```

#### Config Discovery Fallback
```
1. Config file → `~/.looper/config.json`
   └> File not found or invalid JSON
2. Environment variables → `LOOPER_*` env vars
   └> Not set
3. CLI flags → command-line arguments
   └> Not provided
4. Defaults → hardcoded defaults in code
   └> Always available
```

### 2.9 Circuit Breaker (New for Rust)

#### Why Circuit Breaker

Go codebase hien tai khong co circuit breaker. Moi scheduler tick deu co gang goi GitHub API, ke ca khi API dang DOWN. Dieu nay gay ra:

1. **Cascade failures**: Moi tick deu fail → nhieu retry attempts tiep tuc fail → spam logs
2. **Rate limit exhaustion**: Moi tick goi API khi dang bi rate limit → waste remaining quota
3. **Latency accumulation**: Moi tick cho timeout (60s default) → scheduler tick cham di

#### Design

```rust
pub struct CircuitBreakerConfig {
    pub enabled: bool,
    /// So lan that bai lien tiep de mo circuit
    pub failure_threshold: u32,           // default: 5
    /// Thoi gian cho truoc khi chuyen tu Open → HalfOpen
    pub cooldown: Duration,                // default: 60s
    /// Thoi gian cho truoc khi HalfOpen → Open lai (neu that bai)
    pub half_open_timeout: Duration,       // default: 30s
    /// So lan thanh cong trong HalfOpen de dong circuit lai
    pub half_open_success_threshold: u32,  // default: 1
}

pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    state: AtomicU8,  // 0=Closed, 1=Open, 2=HalfOpen
    failure_count: AtomicU32,
    success_count: AtomicU32,
    last_failure_at: AtomicInstant,
    last_success_at: AtomicInstant,
}

impl CircuitBreaker {
    pub fn allow(&self, now: Instant) -> bool {
        match self.state() {
            State::Closed => true,
            State::Open => {
                if now - self.last_failure_at() >= self.config.cooldown {
                    self.set_state(State::HalfOpen);
                    true
                } else {
                    false
                }
            }
            State::HalfOpen => {
                // Allow one probe request
                self.success_count.load(Ordering::Relaxed) < self.config.half_open_success_threshold
            }
        }
    }

    pub fn record_success(&self) {
        self.failure_count.store(0, Ordering::Relaxed);
        self.success_count.fetch_add(1, Ordering::Relaxed);
        if self.success_count() >= self.config.half_open_success_threshold {
            self.set_state(State::Closed);
            self.success_count.store(0, Ordering::Relaxed);
        }
        self.last_success_at.store(Instant::now());
    }

    pub fn record_failure(&self) {
        self.success_count.store(0, Ordering::Relaxed);
        let failures = self.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
        if failures >= self.config.failure_threshold {
            self.set_state(State::Open);
        }
        self.last_failure_at.store(Instant::now(), Ordering::Relaxed);
    }
}
```

**Per-operation circuit breaker sets**:

```rust
pub struct ServiceCircuitBreakers {
    /// Discovery operations: list PRs, list issues
    pub discovery: CircuitBreaker,
    /// Mutation operations: post comment, add label, merge
    pub mutation: CircuitBreaker,
    /// Git operations: fetch, push
    pub git: CircuitBreaker,
}

impl ServiceCircuitBreakers {
    pub fn new(config: &CircuitBreakerConfig) -> Self {
        Self {
            discovery: CircuitBreaker::new(CircuitBreakerConfig {
                failure_threshold: config.failure_threshold,
                cooldown: config.cooldown,
                ..*config
            }),
            mutation: CircuitBreaker::new(CircuitBreakerConfig {
                failure_threshold: config.failure_threshold / 2 + 1, // mutation stricter
                cooldown: config.cooldown * 2, // mutation cools down longer
                ..*config
            }),
            git: CircuitBreaker::new(CircuitBreakerConfig {
                failure_threshold: config.failure_threshold * 2, // git less strict
                cooldown: config.cooldown,
                ..*config
            }),
        }
    }
}
```

**Circuit breaker integration with scheduler**:
```
Scheduler tick start
  └─> Check circuit breaker (discovery)
       ├─ Open → skip all discovery phases
       │   └─> Items remain queued, no new items discovered
       │   └─> Running agents unaffected
       └─> HalfOpen → allow 1 discovery call per project
       └─> Closed → normal operation

Discovery call succeeds → record_success → Closed
Discovery call fails → record_failure → Open (if threshold reached)

Per-project discovery:
  project A fails → circuit breaker records failure
  project B also fails → circuit breaker records another failure
  After 5 total failures → circuit Open → ALL projects skip discovery
  This is a feature: if GitHub is globally down, all projects skip
```

**Integration with retry policy**:
```
Circuit breaker Open
  └─> Scheduler skip discovery
       └─> No new items created
            └─> Existing queued items wait for circuit to close
                 └─> They still have attempts budget
                      └─> Khi circuit closes, attempt count unchanged
```

### 2.10 Notification Throttle (Rust)

```rust
pub struct NotificationThrottle {
    window: Duration,          // default: 60s
    last_sent: Mutex<HashMap<String, Instant>>,
}

impl NotificationThrottle {
    pub fn new(window: Duration) -> Self {
        Self { window, last_sent: Mutex::new(HashMap::new()) }
    }

    /// Kiem tra xem notification co the gui hay khong.
    /// Tra ve `true` neu duoc phep gui.
    pub fn should_send(&self, key: &str, now: Instant) -> bool {
        let mut last_sent = self.last_sent.lock().unwrap();
        if let Some(&last) = last_sent.get(key) {
            if now - last < self.window {
                return false;
            }
        }
        last_sent.insert(key.to_string(), now);
        true
    }

    /// Cleanup old entries de tranh memory leak.
    pub fn cleanup(&self, now: Instant, max_age: Duration) {
        let mut last_sent = self.last_sent.lock().unwrap();
        last_sent.retain(|_, &mut v| now - v < max_age);
    }
}
```

**Dedupe keys**:
```
"loop:{loopID}_failure"
"agent:{executionID}_timeout"
"agent:{executionID}_killed"
"loop:{loopID}_manual_intervention"
"project:{projectID}_startup_failure"
```

### 2.11 Webhook Forwarder (Rust)

```rust
pub struct WebhookForwarderConfig {
    pub queue_capacity: u32,          // default: 128
    pub max_concurrent_workers: u32,  // default: 4
    pub delivery_ttl: Duration,       // default: 1 hour
    pub max_retries: u32,             // default: 2
    pub retry_delay: Duration,        // default: 2s
}

pub struct WebhookForwarder {
    config: WebhookForwarderConfig,
    queue: Queue<DeliveryRequest>,
    in_flight: AtomicU32,
    workers: Vec<WorkerHandle>,
}

impl WebhookForwarder {
    /// Forward mot delivery request.
    /// Reject neu queue full (InFlight >= QueueCapacity).
    pub fn forward(&self, req: DeliveryRequest) -> Result<ForwardResult, WebhookError> {
        if self.in_flight.load(Ordering::Acquire) >= self.config.queue_capacity {
            return Ok(ForwardResult {
                status: "rejected".into(),
                reason: "queue_full".into(),
                work_items: 0,
            });
        }
        self.enqueue(req);
        Ok(ForwardResult {
            status: "accepted".into(),
            reason: "".into(),
            work_items: 1,
        })
    }
}
```

### 2.12 Retry-After Header Parser

```rust
#[derive(Debug, Clone, Copy)]
pub struct RetryAfter {
    pub seconds: u64,
    pub source: RetryAfterSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAfterSource {
    /// Tu GitHub API response header
    ResponseHeader,
    /// Tu error message content
    ErrorMessage,
    /// Default value (secondary rate limit)
    Default,
}

/// Parse retry-after tu nhieu nguon.
pub fn parse_retry_after(stderr: &str, stdout: &str) -> Option<Duration> {
    // 1. Check stderr for retry-after pattern
    let re = Regex::new(r"(?i)retry-after\s*[:=]\s*(\d+)").ok()?;
    if let Some(caps) = re.captures(stderr) {
        if let Ok(secs) = caps.get(1)?.as_str().parse::<u64>() {
            return Some(Duration::from_secs(secs));
        }
    }

    // 2. Check stdout (JSON response)
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(stdout) {
        if let Some(ra) = json.get("retry_after").and_then(|v| v.as_u64()) {
            return Some(Duration::from_secs(ra));
        }
    }

    // 3. Check for rate limit messages with implied delay
    let combined = format!("{}\n{}", stderr, stdout).to_lowercase();
    if combined.contains("secondary rate limit") {
        return Some(Duration::from_secs(60)); // default 60s
    }
    if combined.contains("rate limit") {
        return Some(Duration::from_secs(30)); // default 30s
    }

    None
}
```

### 2.13 Agent Timeout & Idle Detection

```rust
#[derive(Debug, Clone)]
pub struct AgentTimeoutConfig {
    pub max_runtime: Duration,    // Per runner type
    pub idle_timeout: Duration,   // Per runner type
    pub grace_period: Duration,   // Default: 5s (SIGTERM -> SIGKILL)
}

impl AgentTimeoutConfig {
    pub fn for_planner() -> Self {
        Self {
            max_runtime: Duration::from_secs(3600),
            idle_timeout: Duration::from_secs(600),
            grace_period: Duration::from_secs(5),
        }
    }

    pub fn for_worker() -> Self {
        Self {
            max_runtime: Duration::from_secs(10800),
            idle_timeout: Duration::from_secs(900),
            grace_period: Duration::from_secs(5),
        }
    }

    pub fn for_reviewer() -> Self {
        Self {
            max_runtime: Duration::from_secs(5400),
            idle_timeout: Duration::from_secs(600),
            grace_period: Duration::from_secs(5),
        }
    }

    pub fn for_fixer() -> Self {
        Self {
            max_runtime: Duration::from_secs(7200),
            idle_timeout: Duration::from_secs(600),
            grace_period: Duration::from_secs(5),
        }
    }
}
```

**Idle heartbeat detection**:
```
Agent spawn → monitor stdout/stderr
  └─> On each output: reset idle timer
  └─> No output for idle_timeout → begin shutdown:
       1. Log warning: "agent idle timeout exceeded ({idle_timeout})"
       2. SIGTERM to process group
       3. Wait grace_period (5s)
       4. If still alive → SIGKILL
       5. Result: timeout (idle)
```

**Max runtime detection**:
```
Agent spawn → start max_runtime timer
  └─> Timer expires → begin shutdown (same SIGTERM → SIGKILL)
  └─> Even if agent is still producing output
  └─> Result: timeout (max_runtime)
```

### 2.14 Reviewer Loop Throttling

```rust
pub struct ReviewerThrottle {
    pub quiet_period: Duration,          // default: 60s
    pub min_publish_interval: Duration,   // default: 300s
}

impl ReviewerThrottle {
    /// Kiem tra xem co the publish review sau PR change khong.
    /// Tra ve `true` neu duoc phep publish.
    pub fn can_publish_after_change(&self, last_change_at: Instant, now: Instant) -> bool {
        now - last_change_at >= self.quiet_period
    }

    /// Kiem tra xem co the publish review sau lan publish truoc khong.
    pub fn can_publish_after_last(&self, last_publish_at: Instant, now: Instant) -> bool {
        now - last_publish_at >= self.min_publish_interval
    }
}
```

### 2.15 Daemon Restart Throttle

```rust
pub struct DaemonRestartThrottle {
    pub interval: Duration,  // default: 10s
    last_restart: AtomicInstant,
    restart_count: AtomicU32,
}

impl DaemonRestartThrottle {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_restart: AtomicInstant::new(Instant::now()),
            restart_count: AtomicU32::new(0),
        }
    }

    /// Kiem tra da du thoi gian tu lan restart truoc chua.
    pub fn can_restart(&self) -> bool {
        let now = Instant::now();
        if now - self.last_restart.load() >= self.interval {
            self.last_restart.store(now);
            self.restart_count.fetch_add(1);
            true
        } else {
            false
        }
    }

    /// Reset counter sau khi daemon chay on dinh.
    pub fn reset(&self) {
        self.restart_count.store(0);
    }
}
```

---

## 3. Database Schema Changes (If Any)

### 3.1 Circuit Breaker State (optional persist)

```sql
-- Circuit breaker state (optionally persisted across restarts)
CREATE TABLE IF NOT EXISTS circuit_breaker_state (
    name           TEXT PRIMARY KEY,    -- "github_discovery", "github_mutation", "git"
    state          TEXT NOT NULL,       -- "closed", "open", "half_open"
    failure_count  INTEGER NOT NULL DEFAULT 0,
    last_failure_at TEXT,               -- ISO 8601 timestamp
    last_success_at TEXT,               -- ISO 8601 timestamp
    updated_at     TEXT NOT NULL
);
```

Circuit breaker state co the persist de tranh reset khi daemon restart (restart nhanh → circuit van Open → khong spam API). Nhung vi circuit breaker tu dong chuyen HalfOpen sau cooldown (60s), khong can persist cho startup binh thuong. Persist chi huu ich neu restart qua nhanh (< cooldown).

**Recommendation**: in-memory only cho phase 1, persist optional cho phase 2.

---

## 4. Config Surface Changes

### 4.1 Circuit Breaker Config

```rust
// Them vao daemon config
pub struct CircuitBreakerConfig {
    pub enabled: bool,                      // Default: false (opt-in)
    pub failure_threshold: u32,             // Default: 5
    pub cooldown_seconds: u32,              // Default: 60
    pub half_open_timeout_seconds: u32,     // Default: 30
    pub half_open_success_threshold: u32,   // Default: 1
}
```

### 4.2 Enhanced Transient Classification

```rust
// Giong Go: opt-in, off by default
pub struct RetryConfig {
    pub enhanced_transient_classification: bool,  // Default: false
    // ... other retry fields
}
```

---

## 5. Test Scenarios

### 5.1 Rate Limit Tests

```
Test: GitHub returns 429 with Retry-After: 3600
Expected:
  - detect_rate_limit() returns Some(RateLimitInfo { kind: Primary, retry_after: 3600s })
  - classify_failure() → RetryableTransient
  - Queue item retry_at = now + 3600s
  - Circuit breaker records failure
  - No more GitHub calls before retry_at

Test: GitHub returns secondary rate limit
Expected:
  - detect_rate_limit() returns Some(RateLimitInfo { kind: Secondary, retry_after: None })
  - Default retry_after = 60s
  - classify_failure() → RetryableTransient
```

### 5.2 Circuit Breaker Tests

```
Test: 5 consecutive discovery failures
Expected:
  - Circuit breaker state: Open
  - Scheduler tick skip discovery phase
  - allow_request() returns false
  - After 60s cooldown: allow_request() returns true (HalfOpen)

Test: HalfOpen → probe succeeds → Closed
Expected:
  - Circuit breaker state: Closed
  - Normal operation resumes
  - failure_count reset to 0

Test: HalfOpen → probe fails → Open again
Expected:
  - Circuit breaker state: Open
  - Cooldown timer reset
  - failure_count continues from previous count
```

### 5.3 Disk Full Tests

```
Test: SQLite write fails with disk full
Expected:
  - classify_failure() → NonRetryable (Storage)
  - Queue item → ManualIntervention
  - Log warning to stderr (stderr may still work if on different mount)

Test: Log write fails with disk full
Expected:
  - Log rotation fails
  - Warning to stderr: "failed to write log"
  - Agent output captured in in-memory buffer (256KB)
  - looperd continues running
```

### 5.4 Agent Crash Recovery Tests

```
Test: Agent exits with SIGKILL (OOM)
Expected:
  - Shell execution detects exit signal
  - Result.status = "killed"
  - classify_failure() → RetryableTransient (AgentProcess)
  - Native resume attempt (if enabled AND supported)
  - If resume fails → checkpoint restart
  - New Run created for retry

Test: Agent produces no output then exits
Expected:
  - Stdout empty, stderr empty
  - ParseStatus = "missing" (no completion marker)
  - classify_failure() → RetryableTransient
  - Retry with exponential backoff
```

### 5.5 Webhook Overload Tests

```
Test: 200 concurrent deliveries to 128 queue
Expected:
  - First 128 → accepted
  - Next 72 → rejected (queue_full)
  - 4 workers process deliveries concurrently
  - Delivery TTL check: items > 1h old → dropped

Test: Webhook delivery fails twice (max retries)
Expected:
  - Attempt 1: fail → retry after 2s
  - Attempt 2: fail → retry after 2s
  - Attempt 3: fail → permanent failure, drop
  - Event logged: "delivery.failed_permanent"
```

### 5.6 Git Lock Contention Tests

```
Test: git lock held for 2 seconds
Expected:
  - Retry 1: 50ms → locked
  - Retry 2: 100ms → locked
  - Retry 3: 250ms → success (lock released)
  - classify = classified normally (git_remote)

Test: git lock held for 10 seconds (persistent)
Expected:
  - Retry 1-3: all fail
  - classify = RetryableTransient (git_remote)
  - Queue item retried at next scheduler tick
  - After max_attempts → ManualIntervention
```

---

## 6. Edge Cases & Invariants

### 6.1 Rate Limit & Circuit Breaker Interaction

```
Edge case: GitHub rate-limited → circuit breaker Open → retry-after expires
Expected:
  - Rate limit error → classify RetryableTransient
  - Circuit breaker failure_count++
  - When retry-after expires: queue item eligible for next claim
  - Scheduler tick: circuit HalfOpen → allow probe
  - If succeeded: circuit Closed, item claimed
  - If still rate-limited: circuit Open again, new retry-after
```

### 6.2 Multiple Concurrent Rate Limits

```
Edge case: Both discovery and mutation rate-limited simultaneously
Expected:
  - Discovery circuit: Open (5 discovery failures)
  - Mutation circuit: Open (3 mutation failures, stricter threshold)
  - Scheduler tick: skip discovery completely
  - Running agents continue mutations via their own sessions
  - Daemon itself does no mutation (circuit Open)
```

### 6.3 Daemon Restart During Retry

```
Edge case: looperd restarts while items queued with retry_at in the future
Expected:
  - Recovery pipeline: Phase 4 (loop state normalization) runs
  - Queue items preserve retry_at (persisted in SQLite)
  - Scheduler starts: items with retry_at > now are skipped
  - Items are naturally retried when retry_at passes
```

### 6.4 Agent Dies After Side Effects

```
Edge case: Agent pushes to remote but dies before writing completion marker
Expected:
  - Shell: exit code != 0, no completion marker
  - classify_failure() → RetryableTransient (AgentProcess)
  - Native resume: agent resumes with in-progress work
  - Git history already has the push → agent sees it on resume
  - If checkpoint restart: agent starts from last checkpoint
```

### 6.5 Concurrent Circuit Breaker Access

```
Edge case: Multiple scheduler goroutines access circuit breaker concurrently
Expected:
  - Circuit breaker state: Atomic operations (AtomicU32, AtomicInstant)
  - No mutex needed for state transitions
  - allow_request() + record_failure() race: at most 1 extra failure → acceptable
```

---

## 7. Implementation Priority

### Phase 1 (Core)
- `RetryPolicy` struct with delay calculation + jitter
- `FailureKind` enum + `Boundary` enum
- `classify_failure()` function
- Transient error pattern matching
- Retry-after header parser
- Scheduler overload guard (max concurrent + slow lane)

### Phase 2 (Recovery Scenarios)
- Agent crash recovery (native resume + checkpoint fallback)
- Git lock retry logic
- Disk full handling (fallback to in-memory buffers)
- Webhook forwarder backpressure

### Phase 3 (Circuit Breaker)
- `CircuitBreaker` struct + state machine
- Per-operation circuit breakers (discovery, mutation, git)
- Scheduler integration (skip discovery when circuit Open)
- Config surface + opt-in by default

### Phase 4 (Polish)
- Notification throttle
- Reviewer loop throttling (quiet period, publish interval)
- Daemon restart throttle
- Enhanced transient classification (opt-in)
