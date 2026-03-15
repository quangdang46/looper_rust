# PLAN.md — grove

> Beads-backed Rust orchestrator for long-running Claude coding workflows.
> Grove depends on `br` for issue/dependency state and `bv` for graph analysis and triage.
> Grove must **not** depend on any external orchestration, memory, or task CLI besides `br`, `bv`, and `claude`.
> The only other required runtime dependency for MVP is **Claude Code CLI**.

---

## 0. Lock the Direction

This plan intentionally corrects the earlier fully-native task-graph direction.

### Hard rule

Grove must **not**:

- reimplement a second competing task tracker when `br` already owns issues and dependencies
- depend on any external orchestration, memory, or session-search CLI
- become a federation of unrelated subprocess tools glued together by Rust

### Required runtime dependencies

Grove is allowed to depend on:

- `br` as the authoritative issue/dependency backend
- `bv` as the graph analysis / triage backend
- `claude` CLI as the execution backend
- native filesystem access
- native SQLite for grove-owned runtime state

### Ownership split

`br` / `.beads` own:

- issue definitions
- dependency edges
- issue lifecycle metadata
- comments and audit trail when grove annotates progress

`bv` owns:

- graph analytics
- triage recommendations
- critical-path / bottleneck / track insights

Grove owns natively:

- run/session/checkpoint state
- transcript archive
- local retrieval
- compact playbook memory
- reservation / parallel safety
- prompt materialization
- crash recovery
- orchestration policy around the beads graph

### Why

This keeps the product aligned with the confirmed direction:

- reuse proven task graph tooling instead of rebuilding it
- avoid external orchestration/memory/search dependency sprawl
- keep grove focused on autonomous Claude execution, memory, and recovery
- keep the user story simple and consistent with the existing beads workflow

---

## 1. Core Design Patterns

This section documents the **core algorithms, data models, and state machines** that grove implements natively in Rust. Each subsection covers a major subsystem with full implementation detail.

## 1.1 Autonomous Loop, Response Analysis & Circuit Breaker

Grove's core execution loop runs Claude Code sessions with **response analysis**, a **circuit breaker state machine**, **conservative completion gating**, and **integrity checks** — all implemented natively in Rust.

### 1.1.1 Response Analyzer

The analyzer parses Claude Code output (JSON or text) and produces a normalized `IterationAnalysis` record per loop tick.

**Output format detection** (3 JSON shapes):

```rust
enum ClaudeOutputFormat {
    FlatJson,          // {"result": "...", "exit_code": 0}
    CliObject,         // {"type": "result", "content": ...}
    ArrayOfMessages,   // [{"role": "assistant", "content": ...}]
    PlainText,         // raw stdout
}
```

**Normalized analysis record**:

```rust
struct IterationAnalysis {
    exit_signal: bool,
    has_completion_signal: bool,
    is_test_only: bool,
    is_stuck: bool,
    files_modified: Vec<String>,
    has_permission_denials: bool,
    confidence_score: f32,         // 0.0–1.0
    progress_indicators: u32,
    error_lines: Vec<String>,
    output_format: ClaudeOutputFormat,
}
```

**Signal extraction rules**:

1. **Explicit exit signal**: Looks for `GROVE_EXIT`, `TASK_COMPLETE`, or structured `Exit { value: true }` in protocol markers
2. **Completion language** (heuristic): Regex patterns like `all tests pass`, `implementation complete`, `no remaining work`
3. **Test-only loop**: Detects when Claude is only running tests without making code changes — `files_modified.is_empty() && output mentions test execution`
4. **Progress evidence**: Any of — git diff shows changes, files reported modified, new test passes, build succeeds
5. **Permission denial**: Detects `permission denied`, `Operation not permitted`, or tool use rejections
6. **Stuck detection**: Compares `error_lines` across last 3 iterations using string similarity; if >80% overlap → `is_stuck = true`

**Rolling signal window** — the last 5 iterations maintain:

```rust
struct SignalWindow {
    test_only_loops: VecDeque<bool>,    // cap 5
    done_signals: VecDeque<bool>,       // cap 5
    completion_indicators: VecDeque<bool>, // cap 5
    error_fingerprints: VecDeque<u64>,  // hash of error_lines, cap 3
}
```

### 1.1.2 Circuit Breaker State Machine

Three-state circuit breaker to stop runaway token spending when progress stalls.

```
CLOSED ──(trigger)──▶ OPEN ──(cooldown expires)──▶ HALF_OPEN
   ▲                                                    │
   │                                                    │
   └──────────(progress detected)───────────────────────┘
   ▲                                                    │
   └──────────(no recovery)──── OPEN ◀─────────────────┘
```

**State**:

```rust
struct CircuitBreakerState {
    state: CircuitState,                // Closed, HalfOpen, Open
    consecutive_no_progress: u32,
    consecutive_same_error: u32,
    consecutive_permission_denials: u32,
    last_progress_loop: u32,
    opened_at: Option<DateTime<Utc>>,
    cooldown_minutes: u64,              // default 30
    total_loops: u32,
}
```

**Transition rules**:

| From | Condition | To |
|------|-----------|----|
| Closed | `consecutive_no_progress >= no_progress_threshold` (default 3) | Open |
| Closed | `consecutive_same_error >= same_error_threshold` (default 5) | Open |
| Closed | `consecutive_permission_denials >= permission_denial_threshold` (default 2) | Open |
| Open | `now - opened_at >= cooldown_minutes` | HalfOpen |
| HalfOpen | progress detected (git changes, completion signal, file mods) | Closed (reset all counters) |
| HalfOpen | no recovery in next iteration | Open (reset cooldown timer) |

**Progress detection** (any one resets counters):

- `git diff --stat` shows changes since last check
- `analysis.has_completion_signal == true`
- `analysis.files_modified.len() > 0`
- `analysis.progress_indicators > 0`

### 1.1.3 Main Orchestration Loop

The grove execution loop runs this cycle per iteration:

```
init_session()
loop {
    if circuit_breaker.is_open() && !cooldown_expired() → break with CircuitOpen
    if rate_limit_exceeded() → sleep(backoff) and continue
    if graceful_exit_requested() → break with UserExit

    prompt = materialize_prompt(task, checkpoint, memory)
    result = execute_claude(prompt, timeout)

    analysis = analyze_response(result)
    circuit_breaker.update(analysis)
    signal_window.push(analysis)

    if should_exit(analysis, signal_window) → break with Success
    if should_checkpoint(context_pressure) → save_checkpoint() and spawn_new_session()
}
```

**Completion gate** — requires BOTH:

1. Explicit signal: `analysis.exit_signal == true` OR structured protocol `Exit { value: true }`
2. Heuristic confirmation: `signal_window.completion_indicators` has ≥ `completion_indicator_threshold` (default 2) true values in last `heuristic_window` (default 8) iterations

This prevents premature exit from Claude merely "sounding done".

**Rate limiting**: Tracks hourly Claude invocation count; if exceeded, applies exponential backoff.

**Session lifecycle**: Each session has `{session_id, created_at, last_used, reset_at, reset_reason}` with 24-hour expiry forcing a fresh session.

### 1.1.4 State Integrity Validation

Before each orchestration cycle, verify grove-owned state integrity:

```rust
fn validate_grove_integrity(workspace: &Path) -> Vec<String> {
    let required = [
        ".grove/grove.db",
        ".grove/config.snapshot.json",
    ];
    required.iter()
        .filter(|p| !workspace.join(p).exists())
        .map(|p| p.to_string())
        .collect()
}
```

If any required files are missing, the orchestrator must either repair (re-init) or abort with a clear error — never silently continue with corrupt state.

### 1.1.5 Design Decisions

- Rust native, not Bash scripts
- Single `.grove/grove.db` + structured dirs, not file-per-state sprawl
- Direct process spawning, not tmux shell control
- Typed Rust structs with SQLite persistence, not JSON/jq-driven mutation
- Structured protocol markers, not text scraping

### 1.1.6 Module Mapping

| Concept | Grove module |
|---------|-------------|
| Response analyzer | `grove-session::analysis` |
| Signal window + completion gate | `grove-session::exit_policy` |
| Circuit breaker state machine | `grove-session::circuit_breaker` |
| Output format detection | `grove-session::classifier` |
| Loop discipline + crash recovery | `grove-orchestrator::recovery` |
| State integrity validation | `grove-orchestrator::integrity` |

---

## 1.2 Evidence-Scored Playbook Engine

Grove owns a compact, durable, evidence-scored memory engine for accumulating, scoring, curating, and promoting/demoting "lessons learned" bullets. This section defines the **data model**, **scoring algorithm**, **curation pipeline**, **validation gates**, and **outcome feedback** — all native Rust with SQLite persistence.

### 1.2.1 Data Model

**Playbook bullet** — the core memory unit:

```rust
struct PlaybookBullet {
    id: BulletId,
    text: String,
    scope: BulletScope,      // Global, Workspace, Language, Framework, Bead
    kind: BulletKind,         // Do, Avoid, Prefer, Consider
    bullet_type: BulletType,  // Rule, AntiPattern
    state: BulletState,       // Draft, Active, Retired
    maturity: BulletMaturity, // Candidate, Established, Proven, Deprecated
    source: BulletSource,     // UserProvided, Inferred, LLMExtracted, OutcomeDeduced
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    feedback: Vec<FeedbackEvent>,
    tags: Vec<String>,
    content_hash: String,     // SHA-256 of normalized text
}
```

**Feedback event** — evidence for/against a bullet:

```rust
struct FeedbackEvent {
    kind: FeedbackKind,        // Helpful, Harmful
    source: String,            // session ID, bead ID, or user
    timestamp: DateTime<Utc>,
    context: Option<String>,   // what happened
    weight: f64,               // default 1.0, adjustable
}
```

**Playbook delta** — mutations to the playbook (discriminated union):

```rust
enum PlaybookDelta {
    Add { bullet: PlaybookBullet },
    Helpful { bullet_id: BulletId, feedback: FeedbackEvent },
    Harmful { bullet_id: BulletId, feedback: FeedbackEvent, reason: HarmfulReason },
    Replace { old_id: BulletId, new_bullet: PlaybookBullet },
    Deprecate { bullet_id: BulletId, reason: String },
    Merge { source_ids: Vec<BulletId>, merged_bullet: PlaybookBullet },
}

enum HarmfulReason {
    CausedError,
    WastedTime,
    Contradicted,
    Outdated,
    TooVague,
    TooSpecific,
}
```

### 1.2.2 Scoring Algorithm

**Exponential decay** on feedback age:

```
decayed_value = base_weight * 0.5^(age_days / half_life_days)
```

Where `half_life_days = 30` by default.

**Effective score computation**:

```rust
fn effective_score(bullet: &PlaybookBullet, config: &ScoringConfig) -> f64 {
    let now = Utc::now();
    let half_life = config.half_life_days; // default 30
    let decayed_helpful: f64 = bullet.feedback.iter()
        .filter(|f| f.kind == FeedbackKind::Helpful)
        .map(|f| {
            let age = (now - f.timestamp).num_days() as f64;
            f.weight * 0.5_f64.powf(age / half_life)
        })
        .sum();
    let decayed_harmful: f64 = bullet.feedback.iter()
        .filter(|f| f.kind == FeedbackKind::Harmful)
        .map(|f| {
            let age = (now - f.timestamp).num_days() as f64;
            f.weight * 0.5_f64.powf(age / half_life)
        })
        .sum();
    let harmful_multiplier = config.harmful_multiplier; // default 4.0
    let maturity_scale = match bullet.maturity {
        BulletMaturity::Candidate => 0.5,
        BulletMaturity::Established => 1.0,
        BulletMaturity::Proven => 1.5,
        BulletMaturity::Deprecated => 0.0,
    };
    (decayed_helpful - harmful_multiplier * decayed_harmful) * maturity_scale
}
```

**Maturity state machine**:

```
Candidate --(score > promote_threshold for N events)--> Established
Established --(score > proven_threshold for M events)--> Proven
Any --(harmful_ratio > 0.3)--> Deprecated
Any --(score < 0 sustained)--> demoted one level
Any --(score < -prune_threshold)--> Deprecated (auto-prune)
```

Where:
- `promote_threshold = 2.0`, `proven_threshold = 5.0` (configurable)
- `harmful_ratio = decayed_harmful / (decayed_helpful + decayed_harmful)`
- `prune_threshold = 3.0` (configurable)

**Staleness**: A bullet is stale if `(now - last_feedback_or_creation) > staleness_days` (default 90).

### 1.2.3 Curation Pipeline

The curation pipeline processes incoming `PlaybookDelta` values in four phases:

**Phase 1 — Deduplication**: Exact hash match reinforces existing (adds Helpful feedback). Semantic similarity >= 0.85 (Jaccard) triggers merge/reinforce.

**Phase 2 — Conflict Detection**: Checks negation markers ("never" vs "always"), opposite sentiment, and scope conflicts (same scope, Jaccard overlap 0.1-0.2).

**Phase 3 — Apply Delta**: Add inserts new bullet as Candidate. Helpful/Harmful appends feedback event. Replace deprecates old, inserts new. Deprecate sets state. Merge combines source bullets.

**Phase 4 — Post-process**: Inversion — if `decayed_harmful >= prune_threshold` AND `decayed_harmful > 2x decayed_helpful`, convert Rule to AntiPattern. Then run promotion/demotion via scoring.

**Conflict detection markers**:

```rust
const NEGATIVE_MARKERS: &[&str] = &["never", "avoid", "don't", "do not", "stop", "remove"];
const POSITIVE_MARKERS: &[&str] = &["always", "prefer", "use", "ensure", "require"];
const EXCEPTION_MARKERS: &[&str] = &["unless", "except", "only when", "but not"];
```

**Jaccard similarity** for dedup:

```rust
fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let set_a: HashSet<&str> = a.split_whitespace().collect();
    let set_b: HashSet<&str> = b.split_whitespace().collect();
    let intersection = set_a.intersection(&set_b).count() as f64;
    let union = set_a.union(&set_b).count() as f64;
    if union == 0.0 { 0.0 } else { intersection / union }
}
```

**Decision log**: Each curation step records `{ phase, action, reason, bullet_id }` for auditability.

### 1.2.4 Validation Gate

Before accepting an `Add` delta, the validation pipeline checks evidence from past transcripts.

**Auto-decision rules**:
- `success_count >= 5 AND failure_count == 0` -> ACCEPT
- `failure_count >= 3 AND success_count == 0` -> REJECT
- content < 15 chars -> SKIP (too short)
- no keywords found -> SKIP

**Ambiguous cases** go to LLM validation with verdicts: ACCEPT, REJECT, REFINE (normalized to ACCEPT_WITH_CAUTION).

**Success/failure patterns**:

```rust
const SUCCESS_PATTERNS: &[&str] = &[
    "tests pass", "build succeeded", "fixed", "resolved",
    "works correctly", "no errors", "all green",
];
const FAILURE_PATTERNS: &[&str] = &[
    "error", "failed", "broken", "regression",
    "doesn't work", "bug", "crash",
];
```

Non-`Add` deltas (Helpful, Harmful, etc.) skip validation.

### 1.2.5 Diary and Outcome Feedback

**Diary entry** (fast path, no LLM):

```rust
struct DiaryEntry {
    session_id: String,
    bead_id: Option<BeadId>,
    outcome: SessionOutcome,    // Success, Failure, Mixed, Unknown
    summary: String,
    duration_secs: u64,
    files_touched: Vec<String>,
    error_count: u32,
    had_retries: bool,
    created_at: DateTime<Utc>,
}
```

**Outcome-to-feedback scoring**:

```rust
fn score_implicit_feedback(diary: &DiaryEntry) -> (f64, f64) {
    let mut helpful = 0.0;
    let mut harmful = 0.0;
    match diary.outcome {
        SessionOutcome::Success => helpful += 1.0,
        SessionOutcome::Failure => harmful += 1.0,
        SessionOutcome::Mixed => { helpful += 0.3; harmful += 0.3; },
        SessionOutcome::Unknown => {},
    }
    if diary.duration_secs < 600 { helpful += 0.5; }
    if diary.duration_secs > 3600 { harmful += 0.3; }
    if diary.error_count > 0 { harmful += 0.2 * diary.error_count.min(5) as f64; }
    if diary.had_retries { harmful += 0.3; }
    (helpful.clamp(0.1, 2.0), harmful.clamp(0.1, 2.0))
}
```

After scoring, `FeedbackEvent` entries are appended to all bullets that were active during the session (tracked via a context log of applied `bullet_id`s). Maturity is recalculated after each update.

### 1.2.6 Grove Module Mapping

| Concept | Grove module |
|---------|-------------|
| PlaybookBullet schema | `grove-memory::playbook::model` |
| Scoring + maturity | `grove-memory::playbook::scoring` |
| Curation pipeline | `grove-memory::playbook::curate` |
| Validation gate | `grove-memory::playbook::validate` |
| Diary + outcome feedback | `grove-memory::playbook::diary` |
| Bullet selection for prompts | `grove-memory::playbook::selector` |

---

## 1.3 Transcript Archive & Retrieval

Grove ships a native transcript archive and retrieval layer. This section defines the **normalized data model**, **incremental ingest pipeline**, **FTS-first search strategy**, and **SQLite archive patterns** for indexing and searching past agent sessions.

### 1.3.1 Normalized Data Model

The canonical schema for representing agent conversations across providers:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Agent,
    Tool,
    System,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentKind {
    Cli,        // claude CLI, codex CLI
    VsCode,     // Cursor, Copilot, etc.
    Hybrid,     // mixed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,           // "claude", "codex", "cursor"
    pub kind: AgentKind,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub path: String,           // absolute path to project root
    pub hostname: Option<String>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,             // unique per session
    pub source_id: String,      // original file/session ID from agent
    pub origin_host: Option<String>,
    pub agent: Agent,
    pub workspace: Option<Workspace>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub message_count: u32,
    pub tags: Vec<Tag>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub conversation_id: String,
    pub ordinal: u32,           // position in conversation
    pub role: MessageRole,
    pub content: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub token_estimate: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    pub message_id: String,
    pub text: String,
    pub language: Option<String>,
    pub file_path: Option<String>,
    pub line_range: Option<(u32, u32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub key: String,
    pub value: String,
}
```

### 1.3.2 Incremental Ingest Pipeline

The indexer uses a streaming producer-consumer model:

```
Connector (scan dirs) --> Normalizer (to schema) --> Persister (SQLite)
```

**Connectors** scan agent-specific directories:

| Agent | Session path pattern |
|-------|---------------------|
| Claude CLI | `~/.claude/projects/*/sessions/*.jsonl` |
| Codex | `~/.codex/sessions/*.jsonl` |
| Cursor | `~/.cursor/projects/*/sessions/*` |

**Streaming protocol**:

```rust
enum IndexMessage {
    Batch(Vec<Conversation>),
    ScanError { connector: String, error: String },
    Done { connector: String },
}
```

**Watermark-based incremental ingest**:

```rust
struct IngestWatermark {
    connector: String,
    last_file_path: String,
    last_modified: DateTime<Utc>,
    last_byte_offset: u64,
}
```

On each ingest run, only files modified after the watermark are processed. After successful ingest, the watermark is updated atomically.

**Stale detection**:

```rust
struct StaleDetector {
    last_successful_ingest: DateTime<Utc>,
    consecutive_zero_scans: u32,
}

enum StaleAction {
    None,
    Warn,      // consecutive_zero_scans > 5
    Rebuild,   // consecutive_zero_scans > 20 or data corruption detected
}
```

### 1.3.3 FTS-First Search Strategy

MVP search is lexical-first using SQLite FTS5:

```sql
CREATE VIRTUAL TABLE fts_messages USING fts5(
    content,
    conversation_id UNINDEXED,
    role UNINDEXED,
    tokenize='porter unicode61'
);
```

**Search pipeline**:

```rust
struct SearchQuery {
    text: String,
    filters: SearchFilters,
    limit: usize,
    offset: usize,
}

struct SearchFilters {
    agents: Option<Vec<String>>,
    workspaces: Option<Vec<String>>,
    time_range: Option<(DateTime<Utc>, DateTime<Utc>)>,
    roles: Option<Vec<MessageRole>>,
}

struct SearchResult {
    conversation: Conversation,
    matched_messages: Vec<Message>,
    score: f64,
    highlights: Vec<String>,
}
```

**Result deduplication**: Group by conversation, take highest-scoring message per conversation, sort by aggregate score.

**Future hybrid search** (post-MVP): RRF (Reciprocal Rank Fusion) combining FTS5 results with optional embedding-based semantic search. Not required for phase 1.

### 1.3.4 SQLite Archive Model

**Schema version tracking**: `_schema_version(version, applied_at)` with migration checks on open.

**Connection management**: Single writer, multiple readers via WAL mode. `busy_timeout = 5000` for concurrent access.

**Metadata storage**: MessagePack for compact binary metadata fields, with JSON fallback for debugging.

**Backup strategy**: `VACUUM INTO backup_path` for atomic backups, with cleanup of old backups (keep last 3).

### 1.3.5 Grove Module Mapping

| Concept | Grove module |
|---------|-------------|
| Conversation/Message/Snippet types | `grove-memory::archive::model` |
| Incremental ingest pipeline | `grove-memory::archive::ingest` |
| SQLite storage + watermarks | `grove-memory::archive::store` |
| FTS5 search | `grove-memory::archive::fts` |
| Search query + filters | `grove-memory::archive::query` |
| Result retrieval + dedup | `grove-memory::archive::retrieval` |

---

## 1.4 Orchestration State & Dispatch

Grove is bead-centric, not agent-centric. There are no agent types, no agent records, no assignment strategies. The entities are beads, runs, and sessions.

### 1.4.1 Canonical Status Enums

These are defined once in `grove-types` and used everywhere:

```rust
pub enum GroveBeadStatus { Idle, Ready, Running, Checkpointed, WaitingToRetry, Succeeded, Failed }
pub enum RunStatus { Active, WaitingToRetry, Checkpointed, Succeeded, Failed }
pub enum SessionStatus { Starting, Streaming, CheckpointRequested, Completed, TimedOut, RateLimited, PermissionDenied, Crashed, Killed }
pub enum CircuitState { Closed, HalfOpen, Open }
```

No other status enums exist in the codebase. `TaskStatus`, `AgentStatus`, `BeadStatus` from swarm patterns are explicitly not used.

### 1.4.2 Canonical Readiness Rule

> **A bead is dispatchable if and only if `br ready --json` reports it AND grove has no local blocker (active run, retry backoff, breaker open, reservation conflict).**

Grove may only suppress readiness, never create it. If `br` does not report a bead as ready, grove must not dispatch it regardless of local state. The `GroveBeadStatus::Ready` field is a cache of this computed result, not an independent source of truth.

### 1.4.3 Multi-Factor Bead Scoring

The scheduler scores each dispatchable bead to decide dispatch priority:

```rust
struct BeadScore {
    total: i32,
    components: ScoreComponents,
}

struct ScoreComponents {
    base: i32,                    // from bead priority (P0=100, P1=80, P2=60, P3=40, P4=20)
    critical_path_bonus: i32,     // +20 if on critical path (from bv)
    bv_pagerank_bonus: i32,       // +N from bv --robot-priority
    file_overlap_penalty: i32,    // -1000 if reservation conflict exists
    retry_penalty: i32,           // -10 per previous attempt
    ready_age_bonus: i32,         // +1 per minute in ready state
}

fn score_bead(bead: &GroveBeadView, config: &SchedulerConfig) -> BeadScore {
    let mut s = ScoreComponents::default();

    s.base = match bead.bead.priority {
        0 => 100, 1 => 80, 2 => 60, 3 => 40, _ => 20,
    };

    if bead.on_critical_path { s.critical_path_bonus = config.critical_path_bonus; }

    if has_reservation_conflict(bead) {
        s.file_overlap_penalty = -config.reservation_conflict_penalty;
    }

    s.retry_penalty = -(config.retry_penalty as i32) * bead.attempt_count as i32;

    let ready_minutes = bead.ready_since.map(|t| (Utc::now() - t).num_minutes()).unwrap_or(0);
    s.ready_age_bonus = (config.ready_age_bonus_per_min as i64 * ready_minutes) as i32;

    BeadScore {
        total: s.base + s.critical_path_bonus + s.bv_pagerank_bonus
             + s.file_overlap_penalty + s.retry_penalty + s.ready_age_bonus,
        components: s,
    }
}
```

There are no assignment strategies (Balanced, Speed, Quality, RoundRobin). The scheduler scores beads, sorts by score descending, and dispatches up to `max_parallel` concurrency slots.

### 1.4.4 Context Window Monitor

Monitors per-session context usage:

```rust
struct ContextEstimate {
    usage_pct: f32,          // 0.0 to 1.0
    confidence: f32,         // 0.0 to 1.0
    method: String,
}

struct ContextMonitor {
    warn_threshold: f32,     // default 0.70
    rotate_threshold: f32,   // default 0.82
}
```

| Estimator | Method | Confidence |
|-----------|--------|------------|
| RobotMode | Parse `--robot` output for token counts | 0.95 |
| MessageCount | `messages * avg_tokens_per_msg / model_limit` | 0.6 |
| CumulativeToken | Sum estimated tokens from all messages | 0.75 |
| DurationActivity | Time-based heuristic (longer = more tokens) | 0.4 |

**Selection**: Use highest-confidence estimate. If tied, use highest usage_pct (conservative).

**Handoff trigger**:

```rust
fn should_trigger_handoff(monitor: &ContextMonitor, estimate: &ContextEstimate) -> HandoffDecision {
    if estimate.usage_pct >= monitor.rotate_threshold {
        HandoffDecision::RotateNow
    } else if estimate.usage_pct >= monitor.warn_threshold {
        HandoffDecision::Warn
    } else {
        HandoffDecision::Ok
    }
}

enum HandoffDecision { Ok, Warn, RotateNow }
```

| Model | Context window |
|-------|---------------|
| claude-sonnet | 1000k tokens |
| claude-opus | 1000k tokens |
| claude-haiku | 200k tokens |

### 1.4.5 Reservation Conflict Detection

Reservations are per-bead (not per-agent). A bead declares file paths it intends to modify.

```rust
struct ReservationRecord {
    id: i64,
    bead_id: BeadId,
    run_id: RunId,
    path_pattern: String,      // glob pattern
    exclusive: bool,
    expires_at: DateTime<Utc>,
}

fn find_conflicts(db: &Database, bead_id: &BeadId, patterns: &[String]) -> Vec<ReservationRecord> {
    // Find existing exclusive reservations where GLOB patterns overlap,
    // reservation hasn't expired, and belongs to a different bead
}
```

### 1.4.6 Event Bus

Bounded-channel event bus for inter-component communication:

```rust
pub enum OrchestratorEvent {
    BeadDispatched { bead_id: BeadId, run_id: RunId },
    SessionStarted { bead_id: BeadId, session_id: SessionId },
    SessionEnded { bead_id: BeadId, session_id: SessionId, outcome: SessionStatus },
    CheckpointSaved { bead_id: BeadId, checkpoint_id: CheckpointId },
    HandoffPersisted { bead_id: BeadId },
    BeadSucceeded { bead_id: BeadId },
    BeadFailed { bead_id: BeadId, failure: FailureClass },
    ContextPressure { bead_id: BeadId, usage_pct: f32 },
    Shutdown,
}
```

Backed by `tokio::sync::broadcast` with configurable capacity (default 1024).

### 1.4.7 Grove Module Mapping

| Concept | Grove module |
|---------|-------------|
| SQLite orchestration schema | `grove-db::migrations` |
| Event history + replay | `grove-db::event_repo` |
| Reservation records + conflict detection | `grove-db::reservation_repo` |
| Multi-factor bead scoring | `grove-orchestrator::scheduler` |
| Context window monitoring | `grove-session::context_monitor` |
| Event bus | `grove-orchestrator::events` |

---

## 1.5 Design Principle Summary

The valuable patterns captured in Section 1 are:

- domain models (bead, run, session, checkpoint, handoff, bullet, conversation)
- state machine transitions (circuit breaker, run lifecycle, maturity)
- scoring algorithms (bead scoring, evidence scoring, implicit feedback)
- persistence patterns (SQLite, WAL, migrations, watermarks)
- curation logic (dedup, conflict, promotion/demotion)
- retrieval patterns (FTS5, incremental ingest)
- safety logic (circuit breaker, context monitoring)
- recovery logic (event replay, checkpoint/handoff, crash recovery)

What grove explicitly avoids:

- agent types, agent records, or assignment strategies (Balanced/Speed/Quality/RoundRobin)
- wrapping external CLI tools as the primary integration model
- tmux/provider glue for process management
- product dashboards (TUI, web UI, reporting)
- worktree/PR automation as a core concern
- MCP servers or multi-provider abstractions in the kernel

**Therefore grove must be implemented as a native Rust kernel with a small CLI, not as a wrapper of wrappers.**

---

## 2. Product Goal

Grove should let a user do this:

1. define tasks and dependencies in `br`
2. use `bv` to understand graph shape, tracks, bottlenecks, and priority signals
3. run the ready beads with Claude under grove orchestration
4. checkpoint automatically when context pressure gets high
5. pass structured handoffs between completed beads
6. learn from earlier transcripts and lessons using grove's native memory engine
7. continue unattended until the relevant graph is done or a genuine failure requires attention

### User promise

A user should need:

- `grove`
- `br`
- `bv`
- `claude`

No other orchestration/search/task/memory CLI should be required.

---

## 3. Explicit Non-Goals

Not in phase 1:

- reimplementing the beads issue tracker inside grove
- external memory/search tool compatibility as a runtime assumption
- tmux execution
- worktree orchestration
- web UI
- multi-provider runtime abstraction
- role-specialized multi-agent swarm execution
- semantic/vector retrieval as a hard dependency

Also not in phase 1:

- shelling out to git for correctness-critical logic
- relying on `claude --resume` for core correctness

### Important note about `--resume`

**MVP correctness must not depend on Claude CLI session resumption support**.

MVP continuity should work entirely through:

- fresh session spawn
- explicit checkpoint payload
- transcript archive
- structured handoff injection

If `--resume` is later added as an optimization, it must remain optional.

---

## 4. Architectural Principles

## 4.1 Native-first

All core behavior belongs inside grove-owned Rust crates.

## 4.2 One durable source of truth

Use one primary SQLite database at `.grove/grove.db`.

Raw files exist for:

- transcripts
- checkpoints
- artifacts
- append-only logs

But the authoritative runtime state is in SQLite.

## 4.3 Typed state machines

Never model orchestration behavior as stringly-typed shell state when a Rust enum will do.

## 4.4 Protocol-first Claude integration

Do not parse vague English if a protocol marker can be used.

## 4.5 Conservative completion

Never mark success because Claude merely sounds done.

## 4.6 Conservative memory

Never promote a lesson to a strong rule because it appeared once.

## 4.7 Phase complexity intentionally

- sequential kernel first
- archive next
- basic memory next
- parallel scheduler next
- richer curation later
- migration readers later

---

## 5. Repository and Workspace Target Layout

Current repo state is minimal. We should build toward this workspace.

```text
grove/
├── Cargo.toml
├── rust-toolchain.toml
├── grove.toml
├── PLAN.md
├── README.md
├── crates/
│   ├── grove-types/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── ids.rs
│   │       ├── time.rs
│   │       ├── priority.rs
│   │       ├── task.rs
│   │       ├── run.rs
│   │       ├── session.rs
│   │       ├── checkpoint.rs
│   │       ├── handoff.rs
│   │       ├── reservation.rs
│   │       ├── event.rs
│   │       ├── archive.rs
│   │       ├── playbook.rs
│   │       └── errors.rs
│   │
│   ├── grove-config/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── model.rs
│   │       ├── defaults.rs
│   │       ├── loader.rs
│   │       ├── validate.rs
│   │       └── paths.rs
│   │
│   ├── grove-db/
│   │   ├── migrations/
│   │   │   └── 0001_init.sql
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── connection.rs
│   │       ├── migrate.rs
│   │       ├── sqlite.rs
│   │       ├── tx.rs
│   │       ├── task_repo.rs
│   │       ├── run_repo.rs
│   │       ├── session_repo.rs
│   │       ├── checkpoint_repo.rs
│   │       ├── handoff_repo.rs
│   │       ├── reservation_repo.rs
│   │       ├── archive_repo.rs
│   │       ├── playbook_repo.rs
│   │       ├── event_repo.rs
│   │       └── coordinator_repo.rs
│   │
│   ├── grove-kernel/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── services/
│   │       │   ├── mod.rs
│   │       │   ├── task_service.rs
│   │       │   ├── dependency_service.rs
│   │       │   ├── run_service.rs
│   │       │   ├── reservation_service.rs
│   │       │   ├── handoff_service.rs
│   │       │   └── integrity_service.rs
│   │
│   ├── grove-session/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── backend.rs
│   │       ├── protocol.rs
│   │       ├── parser.rs
│   │       ├── analysis.rs
│   │       ├── progress.rs
│   │       ├── exit_policy.rs
│   │       ├── context_monitor.rs
│   │       ├── circuit_breaker.rs
│   │       ├── classifier.rs
│   │       ├── transcript.rs
│   │       ├── prompt_builder.rs
│   │       ├── prompt_materializer.rs
│   │       └── runner.rs
│   │
│   ├── grove-memory/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── archive/
│   │       │   ├── mod.rs
│   │       │   ├── model.rs
│   │       │   ├── ingest.rs
│   │       │   ├── store.rs
│   │       │   ├── fts.rs
│   │       │   ├── query.rs
│   │       │   └── retrieval.rs
│   │       └── playbook/
│   │           ├── mod.rs
│   │           ├── model.rs
│   │           ├── scoring.rs
│   │           ├── curate.rs
│   │           ├── validate.rs
│   │           ├── diary.rs
│   │           ├── outcome.rs
│   │           └── selector.rs
│   │
│   ├── grove-br/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── client.rs
│   │       ├── schema.rs
│   │       ├── sync.rs
│   │       └── mirror.rs
│   │
│   ├── grove-bv/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── client.rs
│   │       ├── triage.rs
│   │       └── schema.rs
│   │
│   ├── grove-orchestrator/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── queue.rs
│   │       ├── scheduler.rs
│   │       ├── dispatch.rs
│   │       ├── reservations.rs
│   │       ├── node_runner.rs
│   │       ├── recovery.rs
│   │       ├── leader.rs
│   │       ├── events.rs
│   │       └── coordinator.rs
│   │
│   └── grove-cli/
│       └── src/
│           ├── main.rs
│           ├── cli.rs
│           ├── output.rs
│           └── commands/
│               ├── init.rs
│               ├── run.rs
│               ├── status.rs
│               ├── log.rs
│               ├── inspect.rs
│               └── retry.rs
└── tests/
    ├── fixtures/
    ├── integration/
    └── golden/
```

---

## 6. Root Cargo Workspace Design

The root `Cargo.toml` should look roughly like this.

```toml
[workspace]
resolver = "2"
members = [
  "crates/grove-types",
  "crates/grove-config",
  "crates/grove-db",
  "crates/grove-kernel",
  "crates/grove-session",
  "crates/grove-memory",
  "crates/grove-br",
  "crates/grove-bv",
  "crates/grove-orchestrator",
  "crates/grove-cli",
]

[workspace.package]
edition = "2024"
license = "MIT"
version = "0.1.0"
authors = ["quangdang"]

[workspace.dependencies]
anyhow = "1"
thiserror = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4", "serde"] }
camino = { version = "1", features = ["serde1"] }
tokio = { version = "1", features = ["full"] }
tokio-stream = "0.1"
futures = "0.3"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["fmt", "env-filter"] }
rusqlite = { version = "0.31", features = ["bundled", "chrono"] }
parking_lot = "0.12"
clap = { version = "4", features = ["derive"] }
regex = "1"
once_cell = "1"
sha2 = "0.10"
blake3 = "1"
smallvec = "1"
walkdir = "2"
ignore = "0.4"
globset = "0.4"
memchr = "2"

[workspace.lints.rust]
unsafe_code = "forbid"

[workspace.lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
```

### Notes

- `rusqlite` with `bundled` avoids system SQLite dependency pain.
- `globset` is enough for reservation overlap matching.
- `ignore` / `walkdir` support future filesystem snapshot logic.
- no HTTP, no web UI, no vector DB dependency in MVP.

---

## 7. `.grove/` Directory Layout

```text
.grove/
├── grove.db
├── config.snapshot.json
├── lock/
│   ├── leader.lock
│   └── state.lock
├── transcripts/
│   └── <bead-id>/
│       └── <session-id>.jsonl
├── checkpoints/
│   └── <bead-id>/
│       └── <checkpoint-id>.json
├── artifacts/
│   └── <bead-id>/
├── logs/
│   └── orchestrator.jsonl
└── tmp/
```

### Rules

- `grove.db` is authoritative.
- transcript files are append-only during a running session, then immutable.
- checkpoint files are written atomically using temp file + rename.
- `.grove/tmp` is disposable.
- grove must never parse its own behavior back out of CLI stdout logs if the structured DB record already exists.

---

## 8. Crate-by-Crate Design

## 8.1 `grove-types`

This crate contains only small, dependency-light shared types.
Everything here should be reusable by `grove-br`, `grove-bv`, `grove-db`, `grove-session`, and `grove-cli` without dragging in IO-heavy dependencies.

File layout matches the canonical file map in Section 22.2: `ids.rs`, `time.rs`, `priority.rs`, `task.rs`, `run.rs`, `session.rs`, `checkpoint.rs`, `handoff.rs`, `reservation.rs`, `event.rs`, `archive.rs`, `playbook.rs`, `errors.rs`.

### `ids.rs`

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BeadId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RunId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CheckpointId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PromptId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TickId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BulletId(pub String);
```

### `priority.rs`

Grove should mirror beads-style priority values rather than inventing another ranking language.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BeadPriority {
    P0,
    P1,
    P2,
    P3,
    P4,
}
```

### Status enums (in `task.rs`, `run.rs`, `session.rs`)

Status enums are distributed across their domain files per the canonical file map (Section 22.2):

- `GroveBeadStatus` → `task.rs`
- `RunStatus`, `FailureClass` → `run.rs`
- `SessionStatus`, `StopReason` → `session.rs`
- `CircuitState` → `session.rs`

See Section 1.4.1 for the canonical enum definitions.

### `protocol.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointPayload {
    pub progress: String,
    pub next_step: String,
    pub context: serde_json::Value,
    pub open_questions: Vec<String>,
    pub claimed_paths: Vec<String>,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProtocolEvent {
    Result { summary: String },
    Artifacts { items: Vec<String> },
    Lessons { items: Vec<String> },
    Decisions { items: Vec<String> },
    Warnings { items: Vec<String> },
    Exit { value: bool },
    Checkpoint { payload: CheckpointPayload },
}
```

### `playbook.rs`

Playbook memory types (adapted from Section 1.2). Canonical file name is `playbook.rs` per Section 22.2.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BulletScope {
    Global,
    Workspace,
    Language,
    Framework,
    Bead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BulletType {
    Rule,
    AntiPattern,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BulletState {
    Draft,
    Active,
    Retired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BulletMaturity {
    Candidate,
    Established,
    Proven,
    Deprecated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeedbackKind {
    Helpful,
    Harmful,
}
```

---

## 8.2 `grove-config`

File layout matches the canonical file map in Section 22.3: `model.rs`, `defaults.rs`, `loader.rs`, `validate.rs`, `paths.rs`.

### Goals

- load `grove.toml`
- resolve relative paths against workspace root
- merge defaults + file + env + CLI overrides
- validate impossible settings

### Main structs (in `model.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroveConfig {
    pub runtime: RuntimeConfig,
    pub scheduler: SchedulerConfig,
    pub checkpoint: CheckpointConfig,
    pub exit_policy: ExitPolicyConfig,
    pub circuit_breaker: CircuitBreakerConfig,
    pub memory: MemoryConfig,
    pub reservations: ReservationConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub claude_bin: String,
    pub default_model: String,
    pub workspace_root: String,
    pub timeout_minutes: u64,
    pub env_passthrough: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerConfig {
    pub max_parallel: usize,
    pub poll_interval_ms: u64,
    pub retry_max: u32,
    pub retry_backoff_secs: u64,
    pub critical_path_bonus: i32,
    pub ready_age_bonus_per_min: i32,
    pub retry_penalty: i32,
    pub reservation_conflict_penalty: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointConfig {
    pub warn_pct: f32,
    pub rotate_pct: f32,
    pub hard_stop_pct: f32,
    pub max_context_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitPolicyConfig {
    pub completion_indicator_threshold: u32,
    pub heuristic_window: usize,
    pub require_explicit_exit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    pub no_progress_threshold: u32,
    pub same_error_threshold: u32,
    pub permission_denial_threshold: u32,
    pub cooldown_minutes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    pub db_path: String,
    pub transcript_dir: String,
    pub archive_top_k: usize,
    pub max_prompt_snippets: usize,
    pub max_prompt_bullets: usize,
    pub enable_playbook: bool,
    pub semantic_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReservationConfig {
    pub enabled: bool,
    pub default_ttl_minutes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
    pub persist_jsonl: bool,
}
```

### Defaults

```toml
[runtime]
claude_bin = "claude"
default_model = "sonnet"
workspace_root = "."
timeout_minutes = 60
env_passthrough = []

[scheduler]
max_parallel = 1
poll_interval_ms = 1000
retry_max = 3
retry_backoff_secs = 30
critical_path_bonus = 20
ready_age_bonus_per_min = 1
retry_penalty = 10
reservation_conflict_penalty = 1000

[checkpoint]
warn_pct = 0.70
rotate_pct = 0.82
hard_stop_pct = 0.90
max_context_bytes = 16000

[exit_policy]
completion_indicator_threshold = 2
heuristic_window = 8
require_explicit_exit = true

[circuit_breaker]
no_progress_threshold = 3
same_error_threshold = 5
permission_denial_threshold = 2
cooldown_minutes = 30

[memory]
db_path = ".grove/grove.db"
transcript_dir = ".grove/transcripts"
archive_top_k = 5
max_prompt_snippets = 3
max_prompt_bullets = 12
enable_playbook = true
semantic_enabled = false

[reservations]
enabled = true
default_ttl_minutes = 60

[logging]
level = "info"
persist_jsonl = true
```

### Validation rules

- `rotate_pct > warn_pct`
- `hard_stop_pct >= rotate_pct`
- `max_parallel >= 1`
- `retry_max >= 1`
- all percentages in `[0.0, 1.0]`
- `completion_indicator_threshold >= 1`
- `cooldown_minutes >= 1`

---

## 8.3 `grove-db`

This crate owns all SQLite persistence: connection management, migrations, and every domain repository.

### Responsibilities

- open SQLite connection and set PRAGMAs
- run migrations
- expose transaction helper
- domain repositories: `task_repo`, `run_repo`, `session_repo`, `checkpoint_repo`, `handoff_repo`, `reservation_repo`, `archive_repo`, `playbook_repo`, `event_repo`, `coordinator_repo`

Each repo file groups SQL by domain. This keeps all SQL in one crate, avoids circular deps, and makes it easy to test persistence in isolation.

### `pragmas.rs`

Use:

```sql
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA synchronous = NORMAL;
PRAGMA temp_store = MEMORY;
PRAGMA busy_timeout = 5000;
```

### Migration strategy

- SQL files in `crates/grove-db/migrations/`
- `_migrations(version INTEGER PRIMARY KEY, name TEXT, applied_at TEXT)`
- apply strictly increasing versions

### `Database` façade

```rust
pub struct Database {
    conn: rusqlite::Connection,
}

impl Database {
    pub fn open(path: &Utf8Path) -> anyhow::Result<Self>;
    pub fn migrate(&mut self) -> anyhow::Result<()>;
    pub fn with_tx<T>(&mut self, f: impl FnOnce(&rusqlite::Transaction<'_>) -> anyhow::Result<T>) -> anyhow::Result<T>;
}
```

---

## 8.4 `grove-kernel`

This crate defines the durable domain model and transition semantics.
It must be explicit that grove owns **runtime** state while `br` owns the authoritative issue graph.

### `bead.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeadRef {
    pub id: BeadId,
    pub title: String,
    pub description: Option<String>,
    pub priority: i32,
    pub issue_type: String,
    pub br_status: String,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroveBeadView {
    pub bead: BeadRef,
    pub grove_status: GroveBeadStatus,
    pub declared_paths: Vec<String>,
    pub metadata: serde_json::Value,
    pub retry_after: Option<DateTime<Utc>>,
    pub last_run_id: Option<RunId>,
    pub last_failure_class: Option<FailureClass>,
    pub last_failure_detail: Option<String>,
}
```

### Meaning

The authoritative issue definition comes from `br`, not grove.
Grove may enrich it with local runtime metadata, but it must not invent a second source of truth for title, description, priority, or dependency state.

### `run.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRun {
    pub id: RunId,
    pub bead_id: BeadId,
    pub attempt_no: u32,
    pub status: RunStatus,
    pub failure_class: Option<FailureClass>,
    pub failure_detail: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub session_count: u32,
    pub checkpoint_count: u32,
    pub last_checkpoint_id: Option<CheckpointId>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FailureClass {
    Timeout,
    RateLimit,
    PermissionDenied,
    CircuitOpen,
    NoProgress,
    RepeatedError,
    ProtocolMalformed,
    ClaudeCrashed,
    BrMirrorFailed,
    Interrupted,
    Unknown,
}
```

### `session.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeSessionRecord {
    pub id: SessionId,
    pub run_id: RunId,
    pub external_session_id: Option<String>,
    pub ordinal_in_run: u32,
    pub status: SessionStatus,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub prompt_bytes: i64,
    pub estimated_input_tokens: i64,
    pub estimated_output_tokens: i64,
    pub exit_code: Option<i32>,
    pub stop_reason: Option<String>,
    pub transcript_path: String,
}
```

### `checkpoint.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: CheckpointId,
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub progress: String,
    pub next_step: String,
    pub payload: serde_json::Value,
    pub saved_at: DateTime<Utc>,
    pub resume_generation: u32,
}
```

### `handoff.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handoff {
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub summary: String,
    pub artifacts: Vec<String>,
    pub lessons: Vec<String>,
    pub decisions: Vec<String>,
    pub warnings: Vec<String>,
    pub completed_at: DateTime<Utc>,
}
```

### `reservation.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reservation {
    pub id: i64,
    pub bead_id: BeadId,
    pub run_id: Option<RunId>,
    pub path_pattern: String,
    pub exclusive: bool,
    pub reason: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub released_at: Option<DateTime<Utc>>,
}
```

### `event.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub id: i64,
    pub kind: EventKind,
    pub bead_id: Option<BeadId>,
    pub run_id: Option<RunId>,
    pub session_id: Option<SessionId>,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EventKind {
    BeadCacheSynced,
    DependencySnapshotSynced,
    GroveStatusUpdated,
    RunStarted,
    SessionStarted,
    ProtocolEvent,
    CheckpointSaved,
    SessionCompleted,
    RunRetried,
    RunFailed,
    HandoffWritten,
    BrMirrorRequested,
    BrMirrorSucceeded,
    BrMirrorFailed,
    ReservationAcquired,
    ReservationReleased,
    RecoveryReconciled,
}
```

### `transition.rs`

This file must encode legal **grove runtime** transitions explicitly.
It must not attempt to replace `br` status semantics.

Examples:

```rust
impl GroveBeadView {
    pub fn can_transition_to(&self, next: GroveBeadStatus) -> bool {
        matches!(
            (self.grove_status, next),
            (GroveBeadStatus::Idle, GroveBeadStatus::Ready)
                | (GroveBeadStatus::Ready, GroveBeadStatus::Running)
                | (GroveBeadStatus::Running, GroveBeadStatus::Checkpointed)
                | (GroveBeadStatus::Checkpointed, GroveBeadStatus::Running)
                | (GroveBeadStatus::Running, GroveBeadStatus::Succeeded)
                | (GroveBeadStatus::Running, GroveBeadStatus::Failed)
                | (GroveBeadStatus::Failed, GroveBeadStatus::Ready)
                | (GroveBeadStatus::WaitingToRetry, GroveBeadStatus::Ready)
        )
    }
}
```

### `store.rs`

The kernel store API should be explicit and transaction-friendly.

```rust
pub trait KernelStore {
    fn upsert_bead_cache(&mut self, bead: &BeadRef) -> anyhow::Result<()>;
    fn list_beads(&self) -> anyhow::Result<Vec<GroveBeadView>>;
    fn get_bead(&self, id: &BeadId) -> anyhow::Result<Option<GroveBeadView>>;
    fn set_grove_status(&mut self, id: &BeadId, status: GroveBeadStatus) -> anyhow::Result<()>;
    fn replace_dependency_snapshot(
        &mut self,
        bead_id: &BeadId,
        parent_ids: &[BeadId],
        child_ids: &[BeadId],
    ) -> anyhow::Result<()>;

    fn create_run(&mut self, run: &TaskRun) -> anyhow::Result<()>;
    fn update_run(&mut self, run: &TaskRun) -> anyhow::Result<()>;
    fn latest_run_for_bead(&self, bead: &BeadId) -> anyhow::Result<Option<TaskRun>>;

    fn create_session(&mut self, session: &ClaudeSessionRecord) -> anyhow::Result<()>;
    fn update_session(&mut self, session: &ClaudeSessionRecord) -> anyhow::Result<()>;

    fn save_checkpoint(&mut self, cp: &Checkpoint) -> anyhow::Result<()>;
    fn latest_checkpoint(&self, bead: &BeadId) -> anyhow::Result<Option<Checkpoint>>;

    fn save_handoff(&mut self, handoff: &Handoff) -> anyhow::Result<()>;
    fn get_handoff(&self, bead: &BeadId) -> anyhow::Result<Option<Handoff>>;

    fn acquire_reservation(&mut self, reservation: &Reservation) -> anyhow::Result<()>;
    fn release_reservation(&mut self, id: i64) -> anyhow::Result<()>;
    fn active_reservations(&self) -> anyhow::Result<Vec<Reservation>>;

    fn append_event(&mut self, event: &EventRecord) -> anyhow::Result<()>;
    fn events_for_bead(&self, bead: &BeadId) -> anyhow::Result<Vec<EventRecord>>;
}
```

---

## 8.5 `grove-session`

This crate is the execution engine around Claude.

## 8.5.1 `backend.rs`

The abstraction must be tiny.

```rust
pub struct StartSessionRequest {
    pub model: String,
    pub prompt: String,
    pub working_dir: Utf8PathBuf,
    pub timeout: Duration,
    pub env: Vec<(String, String)>,
}

pub trait ClaudeBackend: Send + Sync {
    fn start(&self, req: StartSessionRequest) -> anyhow::Result<RunningSession>;
}

pub struct RunningSession {
    pub child: tokio::process::Child,
    pub stdout: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    pub stderr: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStderr>>,
}
```

### MVP backend implementation

`CliClaudeBackend` should spawn:

```text
claude -p <prompt> --model <model>
```

No dependency on `--resume`.
No dependency on structured JSON output for correctness.

If later Claude CLI structured output proves stable enough, we may add it as an optimization path, not a correctness dependency.

## 8.5.2 `protocol.rs`

Canonical user-facing protocol remains line-based markers.

### Accepted markers

- `GROVE_RESULT:`
- `GROVE_ARTIFACTS:`
- `GROVE_LESSONS:`
- `GROVE_DECISIONS:`
- `GROVE_WARNINGS:`
- `GROVE_EXIT:`
- `GROVE_CHECKPOINT:`

### Parsing rules

- only match if the line starts exactly with the marker after trimming leading whitespace
- `GROVE_EXIT:` accepts only `true` or `false` case-insensitive
- `GROVE_CHECKPOINT:` must contain valid JSON object
- `GROVE_ARTIFACTS`, `GROVE_LESSONS`, `GROVE_DECISIONS`, `GROVE_WARNINGS` accept:
  - comma-separated text
  - or JSON array if first non-space char is `[`
- repeated result markers overwrite the last result summary
- repeated list markers merge unique items in order
- malformed marker lines are logged but not fatal unless they block required completion data

## 8.5.3 `parser.rs`

Main parser output:

```rust
#[derive(Debug, Clone, Default)]
pub struct ProtocolState {
    pub result_summary: Option<String>,
    pub artifacts: Vec<String>,
    pub lessons: Vec<String>,
    pub decisions: Vec<String>,
    pub warnings: Vec<String>,
    pub explicit_exit: Option<bool>,
    pub latest_checkpoint: Option<CheckpointPayload>,
    pub events: Vec<ProtocolEvent>,
}
```

## 8.5.4 `analysis.rs`

Parses Claude Code output into a normalized analysis record (see Section 1.1.1).

```rust
#[derive(Debug, Clone)]
pub struct IterationAnalysis {
    pub output_lines: usize,
    pub output_chars: usize,
    pub completion_indicators: u32,
    pub has_explicit_exit_true: bool,
    pub has_explicit_exit_false: bool,
    pub checkpoint_emitted: bool,
    pub probable_progress: ProgressSignal,
    pub permission_denials: u32,
    pub repeated_error_fingerprint: Option<String>,
    pub artifacts_mentioned: Vec<String>,
    pub lessons: Vec<String>,
    pub decisions: Vec<String>,
    pub warnings: Vec<String>,
    pub estimated_prompt_tokens: u32,
    pub estimated_output_tokens: u32,
}
```

### Heuristic completion indicators

Explicit and testable completion-language patterns (see Section 1.1.1).

Examples of indicator phrases in freeform non-protocol lines:

- `all tasks complete`
- `implementation complete`
- `project ready`
- `all done`
- `completed successfully`
- `finished the task`
- `done with this task`

### Important override rule

If `explicit_exit == Some(false)`, then the exit policy must not mark success, even if completion indicators are high.

## 8.5.5 `progress.rs`

Because grove should not rely on git CLI or external file diff tools, progress detection must be native and multi-source.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressSignal {
    None,
    Weak,
    Moderate,
    Strong,
}
```

### Progress sources

Order of trust:

1. explicit protocol evidence
   - result summary present
   - artifacts list non-empty
   - checkpoint with concrete progress and next step
2. textual evidence
   - output length substantial and not repeating same error
3. optional filesystem signal later
   - native workspace snapshot diff, not git CLI

### Initial policy

- `GROVE_RESULT` or non-empty `GROVE_ARTIFACTS` => at least `Moderate`
- valid checkpoint with non-empty progress and next step => at least `Moderate`
- multiple structured markers together => `Strong`
- repeated error with no structured markers => `None`

## 8.5.6 `exit_policy.rs`

```rust
pub enum ExitDecision {
    Continue,
    Success,
}

pub struct ExitPolicy {
    pub completion_indicator_threshold: u32,
    pub require_explicit_exit: bool,
}

impl ExitPolicy {
    pub fn evaluate(&self, analysis: &IterationAnalysis) -> ExitDecision {
        if analysis.has_explicit_exit_false {
            return ExitDecision::Continue;
        }

        if self.require_explicit_exit {
            if analysis.has_explicit_exit_true
                && analysis.completion_indicators >= self.completion_indicator_threshold
            {
                return ExitDecision::Success;
            }
            return ExitDecision::Continue;
        }

        if analysis.completion_indicators >= self.completion_indicator_threshold {
            ExitDecision::Success
        } else {
            ExitDecision::Continue
        }
    }
}
```

## 8.5.7 `context_monitor.rs`

Context pressure monitoring with multiple estimators (see Section 1.4.3).

```rust
pub struct ContextMonitor {
    pub estimated_tokens_in: u32,
    pub estimated_tokens_out: u32,
    pub warn_pct: f32,
    pub rotate_pct: f32,
    pub hard_stop_pct: f32,
    pub model_context_limit: u32,
}
```

### Estimation rules

Phase 1:

- prompt tokens ≈ prompt chars / 4
- output tokens ≈ streamed chars / 4
- total estimated = input + output

### Threshold behavior

- `>= warn_pct` => emit orchestration warning event
- `>= rotate_pct` => prefer graceful checkpoint and respawn
- `>= hard_stop_pct` => kill session if no checkpoint appears within grace window and synthesize emergency checkpoint

## 8.5.8 `circuit_breaker.rs`

Three-state circuit breaker in native Rust (see Section 1.1.2).

```rust
pub struct CircuitBreaker {
    pub state: CircuitState,
    pub no_progress_count: u32,
    pub same_error_count: u32,
    pub permission_denial_count: u32,
    pub last_error_fingerprint: Option<String>,
    pub opened_at: Option<DateTime<Utc>>,
    pub no_progress_threshold: u32,
    pub same_error_threshold: u32,
    pub permission_denial_threshold: u32,
    pub cooldown: Duration,
}
```

### Algorithm

- if no progress this session => `no_progress_count += 1`, else reset
- if same error fingerprint as last time => `same_error_count += 1`, else set to `1`
- if permission denied => `permission_denial_count += 1`, else reset
- if any threshold reached => `Open`
- on startup or before retry, if state `Open` and cooldown elapsed => `HalfOpen`
- one good strong-progress session in `HalfOpen` => `Closed`
- a bad session in `HalfOpen` => back to `Open`

## 8.5.9 `classifier.rs`

This classifier distinguishes recovery policies.

```rust
pub enum SessionTerminalClass {
    Success,
    Checkpoint,
    Timeout,
    RateLimit,
    PermissionDenied,
    Crash,
    UnknownFailure,
}
```

### Classification inputs

- process exit code
- stderr lines
- analysis state
- timeout watcher outcome
- circuit breaker state

### Rate-limit detection

Narrow and testable output classification (see Section 1.1.1).

1. explicit timeout must not be mistaken for rate limit
2. stderr text matching for known rate-limit indicators is a separate branch
3. permission denial has higher priority than generic failure

## 8.5.10 `transcript.rs`

Store all session stream events as JSONL.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TranscriptEvent {
    SessionStarted { session_id: SessionId, ts: DateTime<Utc> },
    StdoutLine { line: String, ts: DateTime<Utc> },
    StderrLine { line: String, ts: DateTime<Utc> },
    ParsedProtocol { event: ProtocolEvent, ts: DateTime<Utc> },
    SessionEnded { exit_code: Option<i32>, ts: DateTime<Utc> },
}
```

### Rules

- flush each line append
- tolerate partial session crash; incomplete transcripts still useful for recovery
- transcript path recorded in `claude_sessions`

## 8.5.11 `runner.rs`

This is the heart of execution.

### Runner result

```rust
pub struct SessionRunOutcome {
    pub terminal_class: SessionTerminalClass,
    pub analysis: IterationAnalysis,
    pub protocol_state: ProtocolState,
    pub transcript_path: Utf8PathBuf,
}
```

### High-level pseudocode

```rust
async fn run_session(req: RunSessionRequest) -> Result<SessionRunOutcome> {
    let mut process = backend.start(req.start)?;
    let mut parser = ProtocolParser::default();
    let mut analysis = IterationAnalysis::default();
    let mut monitor = ContextMonitor::new(...);
    let mut transcript = TranscriptWriter::open(...)?;

    loop {
        tokio::select! {
            stdout = process.next_stdout_line() => { ... }
            stderr = process.next_stderr_line() => { ... }
            _ = timeout_sleep => { ... }
        }
    }
}
```

### Detailed behavior

For each stdout line:

1. append to transcript
2. feed parser
3. update analysis
4. update context monitor
5. if checkpoint marker appears => stop gracefully and return `Checkpoint`
6. if exit policy passes => wait for clean process end or short grace period, then return `Success`

For stderr lines:

1. append to transcript
2. track error fingerprint candidates
3. track permission-denial patterns
4. track rate-limit markers

At timeout:

- kill process
- append timeout end event
- classify `Timeout`

---

## 8.6 `grove-memory`

This crate has two layers.

- transcript archive
- curated playbook memory

## 8.6.1 `archive/model.rs`

Port the normalized data model from Section 1.3 (Coding Agent Session Search extraction).

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Agent,
    Tool,
    System,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: Option<i64>,
    pub bead_id: Option<BeadId>,
    pub run_id: Option<RunId>,
    pub session_id: SessionId,
    pub workspace: Option<Utf8PathBuf>,
    pub title: Option<String>,
    pub source_path: Utf8PathBuf,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub approx_tokens: Option<i64>,
    pub metadata_json: serde_json::Value,
    pub messages: Vec<Message>,
    pub source_id: String,
    pub origin_host: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Option<i64>,
    pub idx: i64,
    pub role: MessageRole,
    pub author: Option<String>,
    pub created_at: Option<i64>,
    pub content: String,
    pub extra_json: serde_json::Value,
    pub snippets: Vec<Snippet>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    pub id: Option<i64>,
    pub file_path: Option<Utf8PathBuf>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub language: Option<String>,
    pub snippet_text: Option<String>,
}
```

## 8.6.2 `archive/ingest.rs`

Since grove owns the transcript format, ingestion is straightforward — no multi-source connector complexity.

### Ingestion pipeline

1. read transcript JSONL
2. create one `Conversation`
3. create synthetic messages:
   - one user/task message from prompt context
   - one or more agent output messages from stdout segments
   - optional system/tool messages from metadata
4. extract snippets/artifact refs if available
5. write `conversations`, `messages`, `snippets`
6. update FTS table

### Important simplification

We do not need to support every third-party agent transcript format in phase 1.
We control the transcript format, so we can normalize deterministically.

## 8.6.3 `archive/fts.rs`

Phase 1 search must be SQLite FTS5 only.

### Search input

- task title
- task description
- parent artifact paths
- declared paths if any

### Search steps

1. derive keywords from title/description
2. query FTS
3. score results with a small native reranker
4. collapse to diverse top snippets

### Rerank fields

- full-text score
- recency bonus
- same workspace bonus
- file path overlap bonus
- same task kind bonus later

## 8.6.4 `archive/retrieval.rs`

```rust
pub struct RelevantSnippet {
    pub conversation_id: i64,
    pub message_id: i64,
    pub file_path: Option<Utf8PathBuf>,
    pub snippet: String,
    pub score: f32,
}

pub struct RetrievalBundle {
    pub snippets: Vec<RelevantSnippet>,
    pub conversations: Vec<i64>,
}
```

### Prompt injection rule

Never dump many transcripts into the prompt.

Default:

- at most `3` snippets
- each snippet truncated to budget
- prefer diversity over redundant hits from the same conversation

## 8.6.5 `playbook/model.rs`

Port the playbook bullet model from Section 1.2.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackEvent {
    pub kind: FeedbackKind,
    pub timestamp: DateTime<Utc>,
    pub bead_id: Option<BeadId>,
    pub run_id: Option<RunId>,
    pub context: Option<String>,
    pub weight: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybookBullet {
    pub id: BulletId,
    pub scope: BulletScope,
    pub scope_key: Option<String>,
    pub category: String,
    pub text: String,
    pub bullet_type: BulletType,
    pub state: BulletState,
    pub maturity: BulletMaturity,
    pub helpful_count: u32,
    pub harmful_count: u32,
    pub feedback_events: Vec<FeedbackEvent>,
    pub confidence_decay_half_life_days: u32,
    pub pinned: bool,
    pub deprecated: bool,
    pub replaced_by: Option<BulletId>,
    pub deprecation_reason: Option<String>,
    pub source_bead_ids: Vec<BeadId>,
    pub source_run_ids: Vec<RunId>,
    pub tags: Vec<String>,
    pub effective_score: Option<f32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

## 8.6.6 `playbook/scoring.rs`

Use decay rather than naive totals.

### Decayed counts

For an event age `d` days and half-life `h`:

```text
decayed_weight = weight * exp(-ln(2) * d / h)
```

### Effective score

```text
effective_score = decayed_helpful - (harmful_multiplier * decayed_harmful)
```

Suggested harmful multiplier:

```text
4.0
```

### Maturity policy

- `Candidate`
  - default for newly learned bullet
- `Established`
  - enough evidence to inject into prompts normally
- `Proven`
  - strong stable rule, high trust
- `Deprecated`
  - no longer trusted, should not be injected except maybe as warning history

Suggested thresholds:

- candidate -> established when:
  - `decayed_helpful >= 3.0`
  - `effective_score > 1.5`
  - harmful ratio below threshold
- established -> proven when:
  - `decayed_helpful >= 10.0`
  - `effective_score > 5.0`
  - harmful ratio very low
- any -> deprecated when:
  - harmful dominates or explicit deprecation delta applied

## 8.6.7 `playbook/curate.rs`

This is the most important memory logic.

### Responsibilities

- exact dedup by normalized text hash
- approximate dedup by token overlap/Jaccard
- conflict detection
- apply deltas
- invert harmful rules into anti-patterns
- prune stale/noisy bullets

### Exact dedup rule

If a candidate lesson matches an existing bullet after normalization:

- do not create a new bullet
- reinforce existing bullet with helpful feedback

### Approximate dedup rule

If token Jaccard similarity exceeds threshold, for example `0.85`:

- reinforce or merge rather than duplicate

### Conflict detection rule

Examples of conflict shape:

- `Always do X`
- `Avoid doing X`

Store both relationship and deprecation decision explicitly.

## 8.6.8 `playbook/validate.rs`

A lesson from one run should not immediately become a strong rule.

### Evidence gate

For MVP:

- lesson from one successful run => candidate only
- repeated recurrence across distinct runs => may activate
- repeated harmful feedback => reject or deprecate

## 8.6.9 `playbook/diary.rs`

Phase 1 diary generation can be heuristic rather than LLM-driven.

### `RunDiary`

```rust
pub struct RunDiary {
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub outcome: SessionOutcome,
    pub summary: String,
    pub accomplishments: Vec<String>,
    pub decisions: Vec<String>,
    pub challenges: Vec<String>,
    pub key_learnings: Vec<String>,
}
```

### MVP source

- `GROVE_RESULT`
- `GROVE_LESSONS`
- `GROVE_DECISIONS`
- `GROVE_WARNINGS`
- failure classification

Later, diaries can be enriched via transcript reflection.

## 8.6.10 `playbook/selector.rs`

At prompt-build time, choose relevant bullets.

### Selection priority

1. exact scope match
2. workspace match
3. tag overlap with bead keywords
4. higher maturity
5. higher effective score
6. recency as tie breaker

### Prompt budget

- maximum `12` bullets
- prefer fewer stronger bullets over many weak ones
- never inject deprecated bullets as positive guidance

---

## 8.7 `grove-orchestrator`

This crate coordinates everything.

## 8.7.1 `queue.rs`

Priority-based queue with bead execution kept local (see Section 1.5.2).

```rust
pub struct QueueEntry {
    pub bead_id: BeadId,
    pub score: i32,
    pub ready_since: DateTime<Utc>,
    pub attempt_count: u32,
}
```

### Queue properties

- recomputed each tick from durable state, not a fragile in-memory source of truth
- can still use an in-memory heap for efficiency during a tick
- durable state remains SQLite

## 8.7.2 `scheduler.rs`

### Responsibilities

- compute ready set
- compute critical-path metric
- compute dispatch score
- filter by reservation conflicts
- produce dispatch plan

### Ready criteria

A task is ready when:

- status is `Pending`, `Ready`, or `Failed` with retry permitted
- all parent dependencies are `Succeeded`
- no active run already owns it
- no active exclusive reservation conflict blocks it

### Critical path estimate

Compute longest descendant depth or weighted descendant count.

A simple first version is enough:

```text
critical_path_bonus = number_of_descendants + number_of_direct_children * 2
```

### Score formula

```text
score = base_priority
      + critical_path_bonus
      + waiting_age_bonus
      - retry_penalty
      - reservation_penalty
      - breaker_penalty
```

### Explainability

Every dispatch decision should be explainable in `grove status`.

```rust
pub struct ScoreBreakdown {
    pub base_priority: i32,
    pub critical_path_bonus: i32,
    pub waiting_age_bonus: i32,
    pub retry_penalty: i32,
    pub reservation_penalty: i32,
    pub breaker_penalty: i32,
    pub final_score: i32,
}
```

## 8.7.3 `reservations.rs`

This is native parallel safety.

### Reservation semantics

- tasks may declare path patterns
- reservations can be exclusive or shared
- exclusive + overlap blocks dispatch
- stale reservations auto-expire

### Overlap algorithm

Use `globset` plus exact-path normalization.

Two reservations conflict if:

- both are active
- at least one is exclusive
- patterns overlap by exact match, parent-child path relation, or symmetric glob match

### Symmetric glob rule

Treat overlap as true if either pattern can match the other or both normalize to the same fixed path.

## 8.7.4 `node_runner.rs`

A node runner owns one `TaskRun` and may create multiple Claude sessions over its lifetime.

### Responsibilities

- create run record
- spawn one Claude session
- record session result
- if checkpoint => create fresh session using checkpoint
- if success => persist handoff and mark task succeeded
- if recoverable failure => backoff and retry up to budget
- update reservations throughout

### High-level pseudocode

```rust
async fn run_bead(bead_id: BeadId) -> anyhow::Result<BeadTerminalState> {
    let run = create_or_resume_run(bead_id.clone())?;
    let mut breaker = load_or_init_breaker(run.id.clone())?;
    let mut latest_checkpoint = load_latest_checkpoint(&bead_id)?;

    loop {
        if breaker.is_open_and_not_ready() {
            return Ok(BeadTerminalState::RetryLater);
        }

        let prompt = build_prompt(&bead_id, latest_checkpoint.as_ref())?;
        let session_outcome = session_runner.run(prompt).await?;

        match session_outcome.terminal_class {
            Success => { ... }
            Checkpoint => { ... }
            Timeout | RateLimit | Crash | UnknownFailure => { ... }
            PermissionDenied => { ... }
        }
    }
}
```

## 8.7.5 `recovery.rs`

On startup, grove must reconcile partial state.

### Recovery scan

1. expire stale reservations
2. inspect active runs
3. inspect sessions without terminal status
4. if session subprocess no longer exists:
   - classify as crash/unknown termination
5. if a checkpoint exists for active run:
   - mark task `Checkpointed`
6. if retry budget remains:
   - move task to `Ready`
7. otherwise:
   - mark task `Failed`
8. append recovery events

### Leader lease

Only one active orchestrator should own dispatch in a workspace.

Use `.grove/lock/leader.lock` + heartbeat row in DB.

## 8.7.6 `events.rs`

In-memory event bus for UI/CLI status streaming later.

MVP uses it for:

- live status refresh in `grove run`
- structured logging
- future TUI/web without changing kernel logic

## 8.7.7 `coordinator.rs`

This is the top-level loop.

### Loop steps

```text
startup recovery
-> refresh readiness
-> expire stale reservations
-> compute ready queue
-> dispatch until capacity full
-> observe finished sessions/runs
-> append events
-> sleep poll interval
-> repeat
```

### Important invariant

The coordinator may crash.
The database and transcript/checkpoint files must still be enough to recover.

---

## 8.8 `grove-cli`

This crate should remain thin.

### MVP commands

- `grove init`
- `grove run`
- `grove status`
- `grove log <bead-id>`
- `grove inspect <bead-id>`
- `grove retry <bead-id>`

### Bead management is user-owned

Grove does **not** wrap `br create`, `br dep add`, or any bead creation/mutation commands.
Users manage their beads directly with `br`. Grove only **reads** the `.beads` graph and **mirrors results back** (e.g., `br close`, `br update`, `br comments add`) after task completion.

### Command semantics

#### `grove init`

- create `.grove/`
- create `grove.toml` if absent
- create DB and migrations
- write config snapshot

#### `grove run`

- perform recovery
- acquire leader lease
- start orchestration loop

#### `grove status`

Show:

- ready tasks with score breakdown
- running tasks with session state and context pressure
- checkpointed tasks
- failed tasks with failure class
- reservation conflicts

#### `grove log <bead-id>`

- stream transcript lines and structured events from latest run/session

#### `grove inspect <bead-id>`

Show:

- bead definition
- dependencies and dependents
- run history
- latest checkpoint
- latest handoff
- archive retrieval summary
- selected playbook bullets

---

## 9. SQLite Schema Specification

Grove will use one SQLite database with these tables.

## 9.1 Migrations table

```sql
CREATE TABLE IF NOT EXISTS _migrations (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

## 9.2 Beads cache

Grove should not become the authority for issue definitions, but it still benefits from a local cache of the subset of bead metadata needed for prompt building, status views, and recovery diagnostics.

```sql
CREATE TABLE bead_cache (
  bead_id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  description TEXT,
  priority INTEGER NOT NULL,
  issue_type TEXT NOT NULL,
  status TEXT NOT NULL,
  assignee TEXT,
  labels_json TEXT NOT NULL DEFAULT '[]',
  parent_ids_json TEXT NOT NULL DEFAULT '[]',
  dependency_ids_json TEXT NOT NULL DEFAULT '[]',
  dependent_ids_json TEXT NOT NULL DEFAULT '[]',
  raw_json TEXT NOT NULL,
  synced_at TEXT NOT NULL
);

CREATE INDEX idx_bead_cache_status ON bead_cache(status);
CREATE INDEX idx_bead_cache_priority_status ON bead_cache(priority, status);
```

### Rule

- `br` remains authoritative
- grove cache is for fast reads, prompt assembly, and crash recovery context
- any disagreement between cache and live `br` state should be resolved in favor of live `br`

## 9.3 Grove-owned runtime state

`br` is authoritative for issue lifecycle and dependency truth.
Grove still needs its own durable runtime table for statuses like `Running`, `Checkpointed`, retry cooldowns, last failure class, and locally declared path hints.

```sql
CREATE TABLE bead_runtime (
  bead_id TEXT PRIMARY KEY,
  grove_status TEXT NOT NULL,
  declared_paths_json TEXT NOT NULL DEFAULT '[]',
  metadata_json TEXT NOT NULL DEFAULT '{}',
  last_run_id TEXT,
  retry_after TEXT,
  last_failure_class TEXT,
  last_failure_detail TEXT,
  runtime_updated_at TEXT NOT NULL,
  FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE,
  FOREIGN KEY (last_run_id) REFERENCES task_runs(id) ON DELETE SET NULL
);

CREATE INDEX idx_bead_runtime_status ON bead_runtime(grove_status);
CREATE INDEX idx_bead_runtime_retry_after ON bead_runtime(retry_after);
```

### Rule

- `bead_cache.status` mirrors the live or recently synced `br` issue status
- `bead_runtime.grove_status` is grove's private orchestration status
- user-facing status commands should show both when it improves debugging clarity
- grove must never write fake issue-state transitions into `br` just to satisfy local runtime bookkeeping

## 9.4 Beads-derived dependency snapshot

For explainability and deterministic scheduler inputs, grove may persist the currently observed dependency edges from `br`.

```sql
CREATE TABLE bead_dependencies (
  parent_id TEXT NOT NULL,
  child_id TEXT NOT NULL,
  relation_type TEXT NOT NULL DEFAULT 'blocks',
  synced_at TEXT NOT NULL,
  PRIMARY KEY (parent_id, child_id, relation_type)
);

CREATE INDEX idx_bead_dependencies_child ON bead_dependencies(child_id);
CREATE INDEX idx_bead_dependencies_parent ON bead_dependencies(parent_id);
```

## 9.5 Runs

```sql
CREATE TABLE task_runs (
  id TEXT PRIMARY KEY,
  bead_id TEXT NOT NULL,
  attempt_no INTEGER NOT NULL,
  status TEXT NOT NULL,
  failure_class TEXT,
  failure_detail TEXT,
  started_at TEXT NOT NULL,
  ended_at TEXT,
  session_count INTEGER NOT NULL DEFAULT 0,
  checkpoint_count INTEGER NOT NULL DEFAULT 0,
  last_checkpoint_id TEXT,
  FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE
);

CREATE INDEX idx_task_runs_bead_attempt ON task_runs(bead_id, attempt_no);
CREATE INDEX idx_task_runs_status ON task_runs(status);
```

## 9.5 Sessions

```sql
CREATE TABLE claude_sessions (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  external_session_id TEXT,
  ordinal_in_run INTEGER NOT NULL,
  status TEXT NOT NULL,
  started_at TEXT NOT NULL,
  ended_at TEXT,
  prompt_bytes INTEGER NOT NULL DEFAULT 0,
  estimated_input_tokens INTEGER NOT NULL DEFAULT 0,
  estimated_output_tokens INTEGER NOT NULL DEFAULT 0,
  exit_code INTEGER,
  stop_reason TEXT,
  transcript_path TEXT NOT NULL,
  FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX idx_claude_sessions_run_ordinal ON claude_sessions(run_id, ordinal_in_run);
CREATE INDEX idx_claude_sessions_external ON claude_sessions(external_session_id);
```

## 9.6 Checkpoints

```sql
CREATE TABLE checkpoints (
  id TEXT PRIMARY KEY,
  bead_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  progress TEXT NOT NULL,
  next_step TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  saved_at TEXT NOT NULL,
  resume_generation INTEGER NOT NULL,
  FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE,
  FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
  FOREIGN KEY (session_id) REFERENCES claude_sessions(id) ON DELETE CASCADE
);

CREATE INDEX idx_checkpoints_bead_saved ON checkpoints(bead_id, saved_at DESC);
CREATE INDEX idx_checkpoints_run_saved ON checkpoints(run_id, saved_at DESC);
```

## 9.7 Handoffs

```sql
CREATE TABLE handoffs (
  bead_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  summary TEXT NOT NULL,
  artifacts_json TEXT NOT NULL,
  lessons_json TEXT NOT NULL,
  decisions_json TEXT NOT NULL,
  warnings_json TEXT NOT NULL,
  completed_at TEXT NOT NULL,
  FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE,
  FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);
```

## 9.8 Reservations

```sql
CREATE TABLE reservations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  bead_id TEXT NOT NULL,
  run_id TEXT,
  path_pattern TEXT NOT NULL,
  exclusive INTEGER NOT NULL,
  reason TEXT,
  expires_at TEXT NOT NULL,
  released_at TEXT,
  FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE,
  FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE SET NULL
);

CREATE INDEX idx_reservations_active ON reservations(released_at, expires_at);
CREATE INDEX idx_reservations_bead ON reservations(bead_id);
```

## 9.9 Event log

```sql
CREATE TABLE event_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL,
  bead_id TEXT,
  run_id TEXT,
  session_id TEXT,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE,
  FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
  FOREIGN KEY (session_id) REFERENCES claude_sessions(id) ON DELETE CASCADE
);

CREATE INDEX idx_event_log_bead ON event_log(bead_id, id);
CREATE INDEX idx_event_log_run ON event_log(run_id, id);
CREATE INDEX idx_event_log_session ON event_log(session_id, id);
CREATE INDEX idx_event_log_kind_created ON event_log(kind, created_at);
```

## 9.10 Conversations

```sql
CREATE TABLE sources (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  label TEXT NOT NULL,
  origin_host TEXT,
  created_at TEXT NOT NULL
);

CREATE TABLE conversations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  bead_id TEXT,
  run_id TEXT,
  session_id TEXT NOT NULL UNIQUE,
  source_id TEXT NOT NULL,
  workspace_path TEXT,
  title TEXT,
  source_path TEXT NOT NULL,
  started_at INTEGER,
  ended_at INTEGER,
  approx_tokens INTEGER,
  metadata_json TEXT NOT NULL,
  FOREIGN KEY (source_id) REFERENCES sources(id),
  FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE SET NULL,
  FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE SET NULL
);

CREATE INDEX idx_conversations_bead ON conversations(bead_id);
CREATE INDEX idx_conversations_run ON conversations(run_id);
```

## 9.11 Messages and snippets

```sql
CREATE TABLE messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  conversation_id INTEGER NOT NULL,
  idx INTEGER NOT NULL,
  role TEXT NOT NULL,
  author TEXT,
  created_at INTEGER,
  content TEXT NOT NULL,
  extra_json TEXT NOT NULL,
  FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
);

CREATE INDEX idx_messages_conv_idx ON messages(conversation_id, idx);

CREATE TABLE snippets (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  message_id INTEGER NOT NULL,
  file_path TEXT,
  start_line INTEGER,
  end_line INTEGER,
  language TEXT,
  snippet_text TEXT,
  FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX idx_snippets_message ON snippets(message_id);
CREATE INDEX idx_snippets_file_path ON snippets(file_path);
```

## 9.12 FTS

```sql
CREATE VIRTUAL TABLE fts_messages USING fts5(
  content,
  author,
  tokenize = 'unicode61'
);
```

### Sync strategy

- insert corresponding `fts_messages` row for each `messages` row
- keep the mapping by `rowid == messages.id`
- no external content table complexity in phase 1

## 9.13 Playbook tables

```sql
CREATE TABLE playbook_bullets (
  id TEXT PRIMARY KEY,
  scope TEXT NOT NULL,
  scope_key TEXT,
  category TEXT NOT NULL,
  text TEXT NOT NULL,
  bullet_type TEXT NOT NULL,
  state TEXT NOT NULL,
  maturity TEXT NOT NULL,
  helpful_count INTEGER NOT NULL DEFAULT 0,
  harmful_count INTEGER NOT NULL DEFAULT 0,
  feedback_events_json TEXT NOT NULL DEFAULT '[]',
  confidence_decay_half_life_days INTEGER NOT NULL DEFAULT 90,
  pinned INTEGER NOT NULL DEFAULT 0,
  deprecated INTEGER NOT NULL DEFAULT 0,
  replaced_by TEXT,
  deprecation_reason TEXT,
  source_bead_ids_json TEXT NOT NULL DEFAULT '[]',
  source_run_ids_json TEXT NOT NULL DEFAULT '[]',
  tags_json TEXT NOT NULL DEFAULT '[]',
  effective_score REAL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (replaced_by) REFERENCES playbook_bullets(id)
);

CREATE INDEX idx_playbook_maturity ON playbook_bullets(maturity);
CREATE INDEX idx_playbook_scope ON playbook_bullets(scope, scope_key);
CREATE INDEX idx_playbook_deprecated ON playbook_bullets(deprecated);
```

```sql
CREATE TABLE feedback_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  bullet_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  weight REAL NOT NULL DEFAULT 1.0,
  bead_id TEXT,
  run_id TEXT,
  context TEXT,
  created_at TEXT NOT NULL,
  FOREIGN KEY (bullet_id) REFERENCES playbook_bullets(id) ON DELETE CASCADE,
  FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE SET NULL,
  FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE SET NULL
);

CREATE INDEX idx_feedback_events_bullet ON feedback_events(bullet_id, created_at DESC);
```

```sql
CREATE TABLE memory_diaries (
  id TEXT PRIMARY KEY,
  bead_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  outcome TEXT NOT NULL,
  summary TEXT NOT NULL,
  accomplishments_json TEXT NOT NULL,
  decisions_json TEXT NOT NULL,
  challenges_json TEXT NOT NULL,
  key_learnings_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE,
  FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);
```

---

## 10. Task Graph Model and Invariants

### Invariant 1

A child task cannot become `Ready` until all parent tasks are `Succeeded`.

### Invariant 2

A task may have many runs, but only one active run at a time.

### Invariant 3

A run may have many Claude sessions, but session ordinals must be strictly increasing.

### Invariant 4

A `Succeeded` task must have exactly one current handoff row.

### Invariant 5

A task in `Checkpointed` status must have a latest checkpoint for its latest active or retryable run.

### Invariant 6

A reservation is active when `released_at IS NULL AND expires_at > now()`.

### Invariant 7

A scheduler tick must never dispatch a task whose reservation conflicts with another active task.

---

## 11. Prompt Building and Context Assembly

## 11.1 Prompt builder inputs

For each task dispatch, prompt assembly uses:

1. task title and description
2. declared paths / reservation hints
3. parent handoffs
4. latest checkpoint if any
5. selected playbook bullets
6. selected transcript snippets
7. grove protocol contract

## 11.2 Prompt segment model

```rust
pub enum PromptSegmentKind {
    Task,
    Reservation,
    ParentHandoff,
    Checkpoint,
    Playbook,
    ArchiveSnippet,
    Protocol,
}

pub struct PromptSegment {
    pub kind: PromptSegmentKind,
    pub priority: u8,
    pub text: String,
    pub estimated_tokens: u32,
}
```

## 11.3 Budget trimming

When prompt is too large:

Drop in this order:

1. lowest-ranked archive snippets
2. lower-ranked playbook bullets
3. verbose older parent handoffs
4. non-essential reservation hints

Never drop:

- task body
- latest checkpoint
- protocol contract

## 11.4 Protocol block

Use this exact style in the prompt:

```text
[GROVE PROTOCOL]
When this task is complete, output:
  GROVE_RESULT: <one line summary>
  GROVE_ARTIFACTS: <comma-separated files or none>
  GROVE_LESSONS: <comma-separated lessons or none>
  GROVE_DECISIONS: <comma-separated decisions or none>
  GROVE_WARNINGS: <comma-separated warnings or none>
  GROVE_EXIT: true

While you are still working, output periodically:
  GROVE_EXIT: false

If context is getting full, emit:
  GROVE_CHECKPOINT: {"progress":"...","next_step":"...","context":{},"open_questions":[],"claimed_paths":[]}
```

---

## 12. Checkpoint, Resume, and Handoff Design

## 12.1 Checkpoint philosophy

Checkpoint is a continuation boundary, not completion.

## 12.2 Explicit checkpoint path

When Claude emits `GROVE_CHECKPOINT`:

1. persist checkpoint row
2. persist checkpoint file atomically
3. mark task `Checkpointed`
4. end current session
5. create next session using checkpoint injection

## 12.3 Emergency checkpoint path

If context pressure crosses `hard_stop_pct` or session crashes near a boundary:

1. synthesize emergency checkpoint from:
   - latest protocol state
   - last known result summary if any
   - last structured output lines
   - prompt/task info
2. persist with a flag in payload: `{"emergency": true, ...}`
3. continue with fresh session if retry budget allows

## 12.4 Resume prompt content

Resume prompt must contain:

- original task summary
- most recent checkpoint progress
- next step
- unresolved blockers/open questions
- relevant parent handoffs again
- compact transcript tail only if needed
- protocol block again

## 12.5 Handoff content

On success persist:

- summary
- artifacts
- lessons
- decisions
- warnings
- completion timestamp

Child tasks consume parent handoffs directly.
They must not scrape parent transcripts by default.

---

## 13. Scheduler, Dispatch, and Parallel Safety

## 13.1 Sequential before parallel

Phase 1 must work with `max_parallel = 1` and still use the same scheduler core.
Parallelism is a later configuration change, not a separate architecture.

## 13.2 Ready set computation

Algorithm:

1. fetch ready beads from `br ready --json`
2. optionally refresh wider issue cache from `br list --json` and targeted `br show --json`
3. optionally fetch graph guidance from `bv --robot-triage`, `bv --robot-next`, or `bv --robot-plan`
4. load active grove runs
5. load active reservations
6. for each ready bead candidate:
   - skip if grove already has an active run for that bead
   - check retry cooldown if the latest grove run failed recently
   - check reservation conflicts
   - add scheduler bonuses/penalties from `bv` insights when available
   - emit dispatch candidate if all pass

### Important rule

Grove should not independently decide that a blocked bead is ready if `br` does not.
`br` is the source of truth for dependency readiness.

## 13.3 Reservation overlap strategy

A declared path like `src/auth/**` conflicts with:

- `src/auth/mod.rs`
- `src/auth/jwt.rs`
- another `src/auth/**`
- `src/**` if either is exclusive

A fixed path like `Cargo.lock` conflicts with:

- exact same path
- broader glob that includes it

## 13.4 Dispatch rules

- highest score wins
- tie break by oldest ready time
- tie break by lexical task id for determinism

## 13.5 Retry policy

Suggested default recovery table:

| Failure class | Retry? | Backoff | Notes |
|---|---:|---:|---|
| Timeout | yes | 30s | maybe rotate session |
| RateLimit | yes | 5m | keep run alive |
| PermissionDenied | no | - | fail fast with actionable reason |
| CircuitOpen | yes | cooldown | allow later half-open retry |
| RepeatedError | bounded | 60s | breaker likely opens |
| ClaudeCrashed | yes | 30s | bounded retries |
| Unknown | bounded | 30s | conservative |

---

## 14. Recovery and Restart Model

Grove must recover from process crash without losing task graph correctness.

## 14.1 Startup recovery algorithm

1. open DB
2. run migrations
3. acquire leader lock or fail if another leader is active
4. expire stale reservations
5. inspect unfinished runs
6. inspect unfinished sessions
7. reconcile statuses:
   - active run + latest checkpoint => `Checkpointed`
   - active run + no checkpoint + crashed session => `Failed` or `Ready` depending on retry budget
8. append recovery events
9. recompute ready tasks
10. enter normal loop

## 14.2 Single-leader rule

Only one orchestrator instance may dispatch tasks for a workspace.

Use:

- OS file lock on `.grove/lock/leader.lock`
- leader heartbeat in DB or lockfile metadata

## 14.3 DB and file consistency rules

- create checkpoint DB row in same logical operation as checkpoint file write
- if file exists but row missing on recovery, import file metadata into DB
- if row exists but file missing, keep row authoritative but mark recovery warning event

---

## 15. Testing Strategy

Testing must focus on the kernel, not only the CLI.

## 15.1 Unit tests

### `grove-types`

- enum serde roundtrips
- id formatting/parsing

### `grove-config`

- defaults
- validation errors
- relative path resolution

### `grove-kernel`

- legal/illegal task transitions
- cycle detection for dependencies
- latest run selection
- reservation active/expired classification

### `grove-session`

- protocol parsing for all marker types
- JSON-array parsing for list markers
- malformed checkpoint handling
- explicit `GROVE_EXIT: false` override
- completion indicator counting
- failure classification
- circuit breaker transitions
- context pressure thresholds

### `grove-memory`

- transcript normalization
- snippet extraction
- FTS retrieval ordering
- exact dedup
- approximate dedup
- decayed scoring
- promotion/demotion
- anti-pattern inversion

### `grove-orchestrator`

- ready set computation
- critical-path bonus calculation
- deterministic score ordering
- reservation conflict blocking
- recovery reconciliation

## 15.2 Integration tests

Use a **fake Claude backend** for almost all integration tests.

Scenarios:

1. single task success
2. parent then child unblocking
3. checkpoint then fresh-session resume
4. timeout then retry
5. rate-limit then retry-later
6. permission denied then fail-fast
7. repeated same error then circuit opens
8. transcript indexed and retrieved by later task
9. repeated lessons promoted to active bullets
10. process crash and startup recovery

## 15.3 Golden tests

Golden fixtures for:

- stdout transcript -> protocol events
- transcript JSONL -> normalized conversation/messages
- retrieval query -> selected snippets
- lesson inputs -> curated bullet outputs
- `grove status` shaping for known DB fixtures

## 15.4 Property tests

- dependency DAG acyclic invariants
- reservation overlap symmetry
- dedup hash idempotence
- recovery idempotence on repeated startup scans

---

## 16. Implementation Sequence

This section is intentionally near-code.

## Phase 1 — Project skeleton and beads integration kernel

### Create crates

- `grove-types`
- `grove-config`
- `grove-db`
- `grove-kernel`
- `grove-cli`
- `grove-br`
- `grove-bv`

### Build first

- root workspace
- migrations runner
- `grove init`
- `br`/`bv` capability checks
- bead cache sync
- `grove status`
- `grove inspect`

### Exact files to create first

- `Cargo.toml`
- `crates/grove-types/src/lib.rs`
- `crates/grove-config/src/lib.rs`
- `crates/grove-db/src/lib.rs`
- `crates/grove-db/migrations/0001_init.sql`
- `crates/grove-kernel/src/lib.rs`
- `crates/grove-br/src/lib.rs`
- `crates/grove-bv/src/lib.rs`
- `crates/grove-cli/src/main.rs`

### Acceptance

- grove validates `br`, `bv`, and `claude` availability
- `.grove/grove.db` created
- grove can read ready beads and cache issue metadata
- status command shows bead-backed readiness correctly

---

## Phase 2 — Claude session runtime

### Build

- `grove-session`
- protocol parser
- transcript writer
- context monitor
- exit policy
- classifier
- simple session runner

### Acceptance

- can run one task against Claude
- transcript file created
- success only when explicit protocol + exit gate satisfied
- timeout/rate-limit/permission-denied classified separately

---

## Phase 3 — Sequential orchestrator

### Build

- `grove-orchestrator`
- run/session/checkpoint persistence
- node runner
- handoff persistence
- startup recovery
- retry/backoff

### Acceptance

- full sequential DAG execution works
- checkpoints respawn new sessions
- parent handoffs unblock children
- orchestrator survives restart and recovers state

---

## Phase 4 — Native archive and lexical retrieval

### Build

- `grove-memory::archive`
- conversation/message/snippet ingest
- FTS search
- prompt retrieval bundle

### Acceptance

- completed task transcripts are searchable
- later tasks receive relevant prior snippets
- no external session-search dependency exists anywhere in runtime

---

## Phase 5 — Basic playbook memory

### Build

- lesson ingestion from `GROVE_LESSONS`
- candidate bullets
- feedback events
- evidence gate
- prompt injection of active bullets

### Acceptance

- repeated lessons become active rules
- one-off noisy lessons remain weak candidates
- no external memory tool dependency exists anywhere in runtime

---

## Phase 6 — Parallel scheduler and reservations

### Build

- bounded concurrency
- reservation manager
- conflict-aware scheduler
- richer `grove status`

### Acceptance

- multiple tasks can run safely in parallel
- overlapping path claims block unsafe dispatch
- recovery preserves consistency after crash

---

## Phase 7 — Rich curation

### Build

- diaries
- deltas
- exact/approximate merge
- anti-pattern inversion
- more nuanced selector rules

### Acceptance

- playbook remains compact over time
- harmful rules can invert into `Avoid` patterns

---

## 17. Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Claude output varies | Protocol markers remain authoritative; English heuristics only assist exit detection |
| Context estimate is noisy | Conservative thresholds + checkpoint early |
| Memory gets noisy | Candidate state + evidence gate + decay + dedup |
| Parallel tasks interfere | Reservation model + deterministic blocking + stale expiry |
| Recovery bugs create duplicated work | Durable run/session/checkpoint records + idempotent recovery scan |
| README still advertises external CLIs | Reconcile README after kernel direction is accepted |
| Overbuilding before shipping | Keep phase order strict: kernel -> session -> orchestrator -> archive -> playbook -> parallel |

---

## 18. README Reconciliation Note

The current `README.md` is directionally closer than this plan was in one important way: it already assumes `br` and `bv` exist in the workflow.

However, it should be updated to remove any mention of external memory/search tool dependencies.

### Required follow-up

README must be rewritten to say:

- grove depends on `br` for issue/dependency state
- grove depends on `bv` for graph-aware triage and planning insight
- grove has its own native archive and memory engine
- grove orchestrates Claude sessions, checkpoints, handoffs, and recovery around the beads graph
- required runtime dependencies are `br`, `bv`, and Claude CLI

This is a documentation follow-up, not a blocker for coding the kernel.

---

## 19. Final Build Rule

When deciding between:

1. using `br` / `bv` for task-graph authority and graph analysis, or
2. rebuilding those surfaces inside grove,

choose `br` / `bv`.

When deciding between:

1. shelling out to external orchestration/memory/search helper tooling, or
2. implementing the proven logic natively in grove-owned Rust modules,

choose grove-owned Rust modules.

For grove MVP, the intentional runtime dependencies are:

- `br`
- `bv`
- `claude`

---

## 20. First Concrete Coding Checklist

If implementation starts immediately, the first exact order should be:

1. create workspace `Cargo.toml`
2. create `grove-types`
3. create `grove-config`
4. create `grove-db` with `0001_init.sql`
5. create `grove-kernel` bead-cache/run/session/checkpoint/handoff/reservation/event models
6. create `grove-cli` with `init`, `run`, `status`, `inspect`, `log`, `retry`
7. create `grove-br` integration layer — **read-only**: `br ready`, `br show`, `br list`; **mirror-only**: `br update`, `br close`, `br comments add`, `br sync --flush-only`
8. create `grove-bv` integration layer for `bv --robot-triage`, `--robot-next`, `--robot-plan`, and `--robot-insights`
9. create `grove-session` backend + protocol parser + transcript writer + exit policy
10. create `grove-orchestrator` sequential node runner + recovery
11. create `grove-memory` archive ingest + FTS
12. create `grove-memory` playbook candidate bullet flow
13. add parallel reservations and bounded concurrency

That order minimizes wasted effort and keeps the product aligned with the confirmed beads-backed direction.

---

## 21. Schema Addendum for Operational Completeness

The earlier schema is enough to explain the product, but near-implementation planning should also define the operational tables needed to make the runtime debuggable and crash-safe.

These are still native grove tables.
They replace the kind of implicit state that wrapper-oriented systems often scatter across lockfiles, tmux state, shell vars, or ad hoc JSON.

## 21.1 Coordinator lease table

The plan already requires:

- a single active leader per workspace
- heartbeat-based recovery
- a visible owner identity for debugging

Add a small lease table.

```sql
CREATE TABLE coordinator_leases (
  workspace_key TEXT PRIMARY KEY,
  owner_id TEXT NOT NULL,
  owner_label TEXT NOT NULL,
  acquired_at TEXT NOT NULL,
  heartbeat_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX idx_coordinator_leases_expires ON coordinator_leases(expires_at);
```

### Semantics

- `workspace_key` is normally the canonicalized absolute workspace path
- only one row exists per workspace
- a lease is considered active when `expires_at > now()`
- heartbeat refresh is an update, not an append-only log
- all lease acquisition must happen inside a DB transaction

### Why keep both file and DB lease

The file lock protects against obvious double-start conditions.
The DB row provides:

- observable owner identity in `grove status`
- expiry timestamps visible after crash
- deterministic takeover conditions
- room for future remote/TUI diagnostics

### Takeover rule

A second coordinator may take over only when:

1. the lease row exists,
2. `expires_at <= now()`, and
3. the file lock can be acquired or recreated safely.

If either condition is ambiguous, the safe action is to refuse leadership and print diagnostics.

## 21.2 Prompt materialization table

When debugging orchestration failures, prompt provenance matters.
Grove should keep a durable record of what it actually asked Claude to do.

```sql
CREATE TABLE prompt_materializations (
  id TEXT PRIMARY KEY,
  bead_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  prompt_path TEXT NOT NULL,
  prompt_hash TEXT NOT NULL,
  byte_count INTEGER NOT NULL,
  segment_manifest_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE,
  FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
  FOREIGN KEY (session_id) REFERENCES claude_sessions(id) ON DELETE CASCADE
);

CREATE INDEX idx_prompt_materializations_bead_created
  ON prompt_materializations(bead_id, created_at DESC);
CREATE INDEX idx_prompt_materializations_run_created
  ON prompt_materializations(run_id, created_at DESC);
```

### Purpose

This table is not for replaying shell commands.
It is for answering questions like:

- what exact prompt produced this checkpoint?
- what archive snippets were injected?
- which playbook bullets were selected?
- how much prompt budget was consumed by context versus instructions?

## 21.3 Dispatch decision table

The scheduler should be explainable.
Instead of recomputing all decisions from logs, persist the key dispatch outputs.

```sql
CREATE TABLE dispatch_decisions (
  id TEXT PRIMARY KEY,
  bead_id TEXT NOT NULL,
  tick_id TEXT NOT NULL,
  disposition TEXT NOT NULL,
  score_breakdown_json TEXT NOT NULL,
  blocking_reasons_json TEXT NOT NULL DEFAULT '[]',
  competing_bead_ids_json TEXT NOT NULL DEFAULT '[]',
  created_at TEXT NOT NULL,
  FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE
);

CREATE INDEX idx_dispatch_decisions_bead_created
  ON dispatch_decisions(bead_id, created_at DESC);
CREATE INDEX idx_dispatch_decisions_tick
  ON dispatch_decisions(tick_id);
```

### `disposition` values

- `dispatched`
- `ready_but_capacity_blocked`
- `blocked_by_dependency`
- `blocked_by_reservation`
- `blocked_by_breaker`
- `blocked_by_retry_backoff`
- `not_ready`

## 21.4 Archive watermarks table

The archive system will eventually have to answer:

- what transcript files were already ingested?
- what was the last processed byte offset or last message index?
- can ingest resume idempotently after a crash?

```sql
CREATE TABLE archive_watermarks (
  source_id TEXT PRIMARY KEY,
  source_kind TEXT NOT NULL,
  source_label TEXT NOT NULL,
  last_cursor TEXT,
  last_ingested_at TEXT,
  metadata_json TEXT NOT NULL DEFAULT '{}'
);
```

### Examples of `last_cursor`

- transcript file path + last JSONL line index
- conversation external id + last message id
- source file digest for immutable imports

## 21.5 Config snapshots table

The runtime config should be durable enough to explain historical behavior.

```sql
CREATE TABLE config_snapshots (
  id TEXT PRIMARY KEY,
  sha256 TEXT NOT NULL,
  source_path TEXT NOT NULL,
  config_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE UNIQUE INDEX idx_config_snapshots_sha256
  ON config_snapshots(sha256);
```

### Why

If a task failed under one retry policy or context threshold, later debugging must know which config produced that behavior.

## 21.6 Integrity checks table

Native state integrity checking before each orchestration cycle (see Section 1.1.4).

```sql
CREATE TABLE integrity_checks (
  id TEXT PRIMARY KEY,
  scope TEXT NOT NULL,
  scope_key TEXT,
  status TEXT NOT NULL,
  findings_json TEXT NOT NULL DEFAULT '[]',
  created_at TEXT NOT NULL
);

CREATE INDEX idx_integrity_checks_scope_created
  ON integrity_checks(scope, scope_key, created_at DESC);
```

### Common integrity scopes

- `workspace`
- `database`
- `task`
- `run`
- `session`
- `archive`
- `playbook`

## 21.7 Migration layout recommendation

Instead of placing every table into one monolithic SQL file forever, split later migrations like this:

```text
crates/grove-db/migrations/
├── 0001_init.sql
├── 0002_coordinator_leases.sql
├── 0003_prompt_materializations.sql
├── 0004_dispatch_decisions.sql
├── 0005_archive_watermarks.sql
└── 0006_config_and_integrity.sql
```

### Rule

`0001_init.sql` may still contain the initial MVP set if implementation begins from scratch.
If development starts incrementally, the addendum tables above can be introduced as follow-up migrations.

---

## 22. Exact File and Module Map

The earlier crate breakdown explains architecture.
This section turns it into a concrete file map so implementation can begin with minimal ambiguity.

## 22.1 Workspace root

```text
grove/
├── Cargo.toml
├── rust-toolchain.toml
├── grove.toml
├── PLAN.md
├── README.md
├── .gitignore
├── crates/
│   ├── grove-types/
│   ├── grove-config/
│   ├── grove-db/
│   ├── grove-kernel/
│   ├── grove-session/
│   ├── grove-memory/
│   ├── grove-br/
│   ├── grove-bv/
│   ├── grove-orchestrator/
│   └── grove-cli/
└── tests/
    ├── fixtures/
    ├── golden/
    └── integration/
```

## 22.2 `crates/grove-types`

```text
crates/grove-types/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── ids.rs
    ├── time.rs
    ├── priority.rs
    ├── task.rs
    ├── run.rs
    ├── session.rs
    ├── checkpoint.rs
    ├── handoff.rs
    ├── reservation.rs
    ├── event.rs
    ├── archive.rs
    ├── playbook.rs
    └── errors.rs
```

### File responsibilities

#### `lib.rs`

- re-export stable public model types
- keep imports ergonomic for downstream crates
- avoid business logic except tiny constructors and helpers

#### `ids.rs`

- newtype IDs for task/run/session/checkpoint/prompt/tick/source/bullet
- parsing helpers
- optional random ID generation helpers

#### `time.rs`

- timestamp aliases/helpers
- monotonic-safe duration helpers where needed
- formatting/parsing helpers for DB and JSON boundaries

#### `priority.rs`

- `Priority` enum
- score helpers
- display helpers for CLI

#### `task.rs`

- `BeadRef`
- `GroveBeadView`
- `GroveBeadStatus`
- bead DTOs shared by CLI, kernel, DB

#### `run.rs`

- `RunStatus`
- `FailureClass`
- `RetryPolicy`
- `TaskRunRecord`

#### `session.rs`

- `ClaudeSessionStatus`
- `StopReason`
- `ClaudeSessionRecord`
- `SessionOutcome`

#### `checkpoint.rs`

- `CheckpointPayload`
- `CheckpointRecord`
- `ResumeGeneration`

#### `handoff.rs`

- `HandoffRecord`
- summary and artifact value types

#### `reservation.rs`

- `ReservationMode`
- `ReservationRecord`
- `ReservationConflict`

#### `event.rs`

- `EventKind`
- `EventLogRecord`
- coordinator and session event payload enums

#### `archive.rs`

- `SourceRecord`
- `ConversationRecord`
- `MessageRecord`
- `SnippetRecord`
- retrieval bundle DTOs

#### `playbook.rs`

- bullet scope, type, state, maturity
- `PlaybookBulletRecord`
- `FeedbackEventRecord`
- `MemoryDiaryRecord`

#### `errors.rs`

- domain error enums shared across crates when they are truly cross-cutting
- avoid putting SQLx-specific or clap-specific errors here

## 22.3 `crates/grove-config`

```text
crates/grove-config/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── model.rs
    ├── defaults.rs
    ├── loader.rs
    ├── validate.rs
    └── paths.rs
```

### File responsibilities

#### `model.rs`

Own all configuration structs.
No IO.
No environment probing.
Pure data model.

#### `defaults.rs`

- default retry budgets
- default context thresholds
- default poll intervals
- default path names inside `.grove/`

#### `loader.rs`

- read `grove.toml`
- merge defaults
- optionally apply env overrides later
- produce immutable runtime config snapshot

#### `validate.rs`

- ensure positive intervals
- ensure retry caps are sane
- ensure thresholds are monotonic
- ensure no invalid path collision in grove-owned directories

#### `paths.rs`

- canonical workspace paths
- `.grove/` path derivation
- transcript/checkpoint/prompt path helpers

## 22.4 `crates/grove-db`

```text
crates/grove-db/
├── Cargo.toml
├── migrations/
│   └── 0001_init.sql
└── src/
    ├── lib.rs
    ├── connection.rs
    ├── migrate.rs
    ├── sqlite.rs
    ├── task_repo.rs
    ├── run_repo.rs
    ├── session_repo.rs
    ├── checkpoint_repo.rs
    ├── handoff_repo.rs
    ├── reservation_repo.rs
    ├── archive_repo.rs
    ├── playbook_repo.rs
    ├── event_repo.rs
    ├── coordinator_repo.rs
    └── tx.rs
```

### Design note

Each repo file should group SQL by domain, not by command.
Avoid one thousand-line `queries.rs` dump.

## 22.5 `crates/grove-kernel`

```text
crates/grove-kernel/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── services/
    │   ├── mod.rs
    │   ├── task_service.rs
    │   ├── dependency_service.rs
    │   ├── run_service.rs
    │   ├── reservation_service.rs
    │   ├── handoff_service.rs
    │   └── integrity_service.rs
    ├── graph/
    │   ├── mod.rs
    │   ├── topo.rs
    │   ├── cycle.rs
    │   └── readiness.rs
    ├── policy/
    │   ├── mod.rs
    │   ├── retry.rs
    │   ├── scoring.rs
    │   └── reservation_rules.rs
    └── queries/
        ├── mod.rs
        ├── status_view.rs
        └── inspect_view.rs
```

### Kernel rule

The kernel owns business invariants.
The DB crate persists facts.
The CLI merely calls services.

## 22.6 `crates/grove-session`

```text
crates/grove-session/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── backend.rs
    ├── invocation.rs
    ├── protocol.rs
    ├── parser.rs
    ├── analysis.rs
    ├── progress.rs
    ├── exit_policy.rs
    ├── context_monitor.rs
    ├── circuit_breaker.rs
    ├── classifier.rs
    ├── transcript.rs
    ├── prompt_builder.rs
    ├── prompt_materializer.rs
    └── runner.rs
```

## 22.7 `crates/grove-memory`

```text
crates/grove-memory/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── archive/
    │   ├── mod.rs
    │   ├── model.rs
    │   ├── ingest.rs
    │   ├── normalize.rs
    │   ├── fts.rs
    │   ├── retrieval.rs
    │   └── watermarks.rs
    └── playbook/
        ├── mod.rs
        ├── model.rs
        ├── scoring.rs
        ├── curate.rs
        ├── validate.rs
        ├── diary.rs
        ├── selector.rs
        └── feedback.rs
```

## 22.8 `crates/grove-orchestrator`

```text
crates/grove-orchestrator/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── queue.rs
    ├── scheduler.rs
    ├── reservations.rs
    ├── node_runner.rs
    ├── recovery.rs
    ├── events.rs
    ├── coordinator.rs
    ├── leader.rs
    ├── tick.rs
    └── backoff.rs
```

## 22.9 `crates/grove-cli`

```text
crates/grove-cli/
├── Cargo.toml
└── src/
    ├── main.rs
    ├── app.rs
    ├── output.rs
    ├── commands/
    │   ├── mod.rs
    │   ├── init.rs
    │   ├── run.rs
    │   ├── status.rs
    │   ├── inspect.rs
    │   ├── retry.rs
    │   └── log.rs
    └── formatting/
        ├── mod.rs
        ├── table.rs
        ├── json.rs
        └── colors.rs
```

## 22.10 `tests/` layout

```text
tests/
├── fixtures/
│   ├── prompts/
│   ├── transcripts/
│   ├── checkpoints/
│   ├── handoffs/
│   ├── conversations/
│   └── playbook/
├── golden/
│   ├── status/
│   ├── inspect/
│   ├── logs/
│   └── prompts/
└── integration/
    ├── init_flow.rs
    ├── dependency_flow.rs
    ├── sequential_run_flow.rs
    ├── checkpoint_resume_flow.rs
    ├── recovery_flow.rs
    ├── archive_flow.rs
    └── reservation_flow.rs
```

---

## 23. Concrete Rust API Sketches

This section is intentionally code-shaped.
It is not meant to be copied blindly.
It is meant to remove ambiguity about what the modules should contain.

## 23.1 ID and timestamp model

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub type Timestamp = DateTime<Utc>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BeadId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CheckpointId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PromptId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TickId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BulletId(pub String);
```

### ID rule

- `BeadId` stores the exact `br` issue ID, e.g. `bd-e9b1d4`
- grove-generated IDs (`run_*`, `ses_*`, `chk_*`, `prm_*`, `tick_*`) should be opaque and stable enough for filenames and logs
- the CLI may accept short prefixes later, but the stored durable value remains full-length

## 23.2 Bead and runtime model sketch

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BeadPriority {
    P0,
    P1,
    P2,
    P3,
    P4,
}

impl BeadPriority {
    pub fn base_score(self) -> i32 {
        match self {
            Self::P0 => 100,
            Self::P1 => 70,
            Self::P2 => 40,
            Self::P3 => 20,
            Self::P4 => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum GroveBeadStatus {
    Idle,
    Ready,
    Running,
    Checkpointed,
    WaitingToRetry,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeadRef {
    pub id: BeadId,
    pub title: String,
    pub description: Option<String>,
    pub priority: i32,
    pub issue_type: String,
    pub br_status: String,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroveBeadRecord {
    pub bead: BeadRef,
    pub grove_status: GroveBeadStatus,
    pub declared_paths: Vec<String>,
    pub metadata: serde_json::Value,
    pub last_run_id: Option<RunId>,
    pub retry_after: Option<Timestamp>,
    pub last_failure_class: Option<FailureClass>,
    pub last_failure_detail: Option<String>,
    pub synced_at: Timestamp,
    pub runtime_updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BeadRuntimePatch {
    pub grove_status: Option<GroveBeadStatus>,
    pub declared_paths: Option<Vec<String>>,
    pub metadata: Option<serde_json::Value>,
    pub last_run_id: Option<Option<RunId>>,
    pub retry_after: Option<Option<Timestamp>>,
    pub last_failure_class: Option<Option<FailureClass>>,
    pub last_failure_detail: Option<Option<String>>,
}
```

### Important status meaning

- `Idle`: grove knows about the bead but it is not currently dispatchable
- `Ready`: live `br ready --json` says it is unblocked and grove has no local blocker
- `Running`: the bead currently owns an active grove run
- `Checkpointed`: the latest run paused with resumable state
- `WaitingToRetry`: grove is intentionally holding until backoff expires
- `Succeeded`: grove persisted the final handoff and intends the bead to be closed in `br`
- `Failed`: grove reached a terminal stop pending explicit retry or user action

## 23.3 Run model sketch

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RunStatus {
    Active,
    WaitingToRetry,
    Checkpointed,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FailureClass {
    Timeout,
    RateLimit,
    PermissionDenied,
    Crash,
    NoProgress,
    SameErrorLoop,
    BrSyncFailed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunRecord {
    pub id: RunId,
    pub bead_id: BeadId,
    pub attempt_no: i32,
    pub status: RunStatus,
    pub failure_class: Option<FailureClass>,
    pub failure_detail: Option<String>,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub session_count: i32,
    pub checkpoint_count: i32,
    pub last_checkpoint_id: Option<CheckpointId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub timeout_backoff_secs: u64,
    pub rate_limit_backoff_secs: u64,
    pub crash_backoff_secs: u64,
    pub no_progress_backoff_secs: u64,
    pub permission_denied_requires_manual_retry: bool,
}
```

## 23.4 Session model sketch

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ClaudeSessionStatus {
    Starting,
    Running,
    Completed,
    Checkpointed,
    TimedOut,
    RateLimited,
    PermissionDenied,
    Crashed,
    UnknownFailure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeSessionRecord {
    pub id: SessionId,
    pub run_id: RunId,
    pub external_session_id: Option<String>,
    pub ordinal_in_run: i32,
    pub status: ClaudeSessionStatus,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub prompt_bytes: i32,
    pub estimated_input_tokens: i32,
    pub estimated_output_tokens: i32,
    pub exit_code: Option<i32>,
    pub stop_reason: Option<String>,
    pub transcript_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionOutcome {
    pub session: ClaudeSessionRecord,
    pub protocol_events: Vec<ProtocolEvent>,
    pub analysis: IterationAnalysis,
    pub terminal_class: SessionTerminalClass,
    pub stdout_tail: Vec<String>,
    pub stderr_tail: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionTerminalClass {
    Success,
    Checkpoint,
    Timeout,
    RateLimit,
    PermissionDenied,
    Crash,
    UnknownFailure,
}
```

## 23.5 Reservation model sketch

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReservationMode {
    Shared,
    Exclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReservationRecord {
    pub id: i64,
    pub bead_id: BeadId,
    pub run_id: Option<RunId>,
    pub path_pattern: String,
    pub mode: ReservationMode,
    pub reason: Option<String>,
    pub expires_at: Timestamp,
    pub released_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReservationConflict {
    pub requested_by_bead: BeadId,
    pub conflicting_bead: BeadId,
    pub requested_pattern: String,
    pub held_pattern: String,
    pub conflicting_run_id: Option<RunId>,
}
```

## 23.6 Prompt segment model sketch

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PromptSegmentKind {
    Instructions,
    BeadDefinition,
    ParentHandoffs,
    ArchiveContext,
    PlaybookRules,
    ResumeCheckpoint,
    ProtocolContract,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSegment {
    pub kind: PromptSegmentKind,
    pub label: String,
    pub content: String,
    pub approx_chars: usize,
    pub priority: u8,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptAssembly {
    pub id: PromptId,
    pub bead_id: BeadId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub segments: Vec<PromptSegment>,
    pub total_chars: usize,
    pub prompt_text: String,
}
```

## 23.7 Event payload model sketch

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventKind {
    BeadCacheSynced,
    DependencySnapshotSynced,
    GroveStatusUpdated,
    RunStarted,
    SessionStarted,
    SessionCheckpointed,
    SessionSucceeded,
    SessionFailed,
    HandoffWritten,
    ReservationGranted,
    ReservationConflictDetected,
    ReservationExpired,
    RecoveryActionTaken,
    LeaseAcquired,
    LeaseHeartbeat,
    LeaseReleased,
    ArchiveIngested,
    PlaybookBulletAdded,
    PlaybookBulletPromoted,
    PlaybookBulletDeprecated,
    BrMirrorRequested,
    BrMirrorSucceeded,
    BrMirrorFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLogRecord {
    pub id: i64,
    pub kind: EventKind,
    pub bead_id: Option<BeadId>,
    pub run_id: Option<RunId>,
    pub session_id: Option<SessionId>,
    pub payload: serde_json::Value,
    pub created_at: Timestamp,
}
```

## 23.8 Repository trait sketch

```rust
pub trait BeadRepository {
    fn upsert_bead_cache(&self, bead: &BeadRef) -> anyhow::Result<()>;
    fn get_bead(&self, id: &BeadId) -> anyhow::Result<Option<GroveBeadRecord>>;
    fn list_beads(&self) -> anyhow::Result<Vec<GroveBeadRecord>>;
    fn update_runtime(&self, id: &BeadId, patch: BeadRuntimePatch) -> anyhow::Result<()>;
    fn replace_dependency_snapshot(
        &self,
        bead_id: &BeadId,
        parent_ids: &[BeadId],
        child_ids: &[BeadId],
    ) -> anyhow::Result<()>;
}

pub trait RunRepository {
    fn create_run(&self, bead_id: &BeadId, attempt_no: i32) -> anyhow::Result<TaskRunRecord>;
    fn get_active_run(&self, bead_id: &BeadId) -> anyhow::Result<Option<TaskRunRecord>>;
    fn update_run_status(&self, run_id: &RunId, status: RunStatus) -> anyhow::Result<()>;
    fn attach_failure(
        &self,
        run_id: &RunId,
        class: FailureClass,
        detail: Option<String>,
    ) -> anyhow::Result<()>;
}

pub trait SessionRepository {
    fn create_session(
        &self,
        run_id: &RunId,
        ordinal_in_run: i32,
        transcript_path: String,
    ) -> anyhow::Result<ClaudeSessionRecord>;
    fn finish_session(&self, outcome: &SessionOutcome) -> anyhow::Result<()>;
}

pub trait CheckpointRepository {
    fn insert_checkpoint(&self, record: CheckpointRecord) -> anyhow::Result<()>;
    fn latest_for_bead(&self, bead_id: &BeadId) -> anyhow::Result<Option<CheckpointRecord>>;
}

pub trait HandoffRepository {
    fn upsert_handoff(&self, handoff: HandoffRecord) -> anyhow::Result<()>;
    fn get_handoff(&self, bead_id: &BeadId) -> anyhow::Result<Option<HandoffRecord>>;
}
```

## 23.9 Service layer sketch

```rust
pub struct BeadSyncService<R, B> {
    repo: R,
    br: B,
}

impl<R: BeadRepository, B: BrClient> BeadSyncService<R, B> {
    pub fn sync_one(&self, bead_id: &BeadId) -> anyhow::Result<GroveBeadRecord> {
        let details = self.br.show_issue(bead_id)?;
        self.repo.upsert_bead_cache(&details.into())?;
        self.repo.replace_dependency_snapshot(bead_id, &details.parent_ids, &details.child_ids)?;
        self.repo
            .get_bead(bead_id)?
            .ok_or_else(|| anyhow::anyhow!("bead disappeared after sync"))
    }
}

pub struct BeadLifecycleService<R> {
    repo: R,
}

impl<R: BeadRepository> BeadLifecycleService<R> {
    pub fn mark_running(&self, bead_id: &BeadId, run_id: &RunId) -> anyhow::Result<()> {
        self.repo.update_runtime(
            bead_id,
            BeadRuntimePatch {
                grove_status: Some(GroveBeadStatus::Running),
                last_run_id: Some(Some(run_id.clone())),
                ..Default::default()
            },
        )
    }
}
```

### Real implementation note

Dependency validation belongs to `br`.
Grove may validate obvious malformed input early, but cycle enforcement must not be reimplemented as a second source of truth.
The local dependency snapshot exists for explainability and prompt assembly, not for authoritative graph mutation.

## 23.10 Ready set computation sketch

```rust
pub struct ReadyCandidate {
    pub bead: GroveBeadRecord,
    pub parent_ids: Vec<BeadId>,
    pub child_ids: Vec<BeadId>,
    pub attempt_count: u32,
    pub waiting_since: Option<Timestamp>,
    pub breaker_penalty: i32,
    pub bv_bonus: i32,
}

pub struct ReadyEvaluation {
    pub bead_id: BeadId,
    pub ready: bool,
    pub blocking_reasons: Vec<String>,
}

pub trait ReadinessEngine {
    fn evaluate(&self, bead_id: &BeadId) -> anyhow::Result<ReadyEvaluation>;
    fn list_ready_candidates(&self) -> anyhow::Result<Vec<ReadyCandidate>>;
}
```

### Ready evaluation order

1. bead exists in local cache
2. live `br ready --json` includes the bead
3. grove does not already have an active run for that bead
4. retry backoff window is satisfied
5. breaker does not forbid immediate restart
6. reservation conflicts do not block dispatch
7. any `bv` guidance is applied only as scoring, never as an override for `br` readiness

## 23.11 Dispatch planning sketch

```rust
pub struct DispatchCapacity {
    pub max_concurrency: usize,
    pub currently_running: usize,
}

pub struct DispatchPlan {
    pub tick_id: TickId,
    pub to_dispatch: Vec<BeadId>,
    pub deferred: Vec<(BeadId, Vec<String>)>,
}

pub trait Scheduler {
    fn plan_dispatch(&self, capacity: DispatchCapacity) -> anyhow::Result<DispatchPlan>;
}
```

---

## 24. Transaction Boundaries and SQL Responsibilities

A durable orchestrator lives or dies by transaction boundaries.
This section defines where the system must commit atomically in a **bead-backed** design.

## 24.1 Bead ownership rule

Bead creation (`br create`) and dependency management (`br dep add`) are **user-owned**.
Grove never calls these commands. Users manage their `.beads` graph directly with `br`.

Grove only:
- **reads** the bead graph via `br ready`, `br show`, `br list`
- **mirrors results** after task completion via `br close`, `br update`, `br comments add`

## 24.2 `open run` transaction

Within one transaction:

1. assert bead is eligible for run creation
2. compute next attempt number
3. insert `task_runs`
4. upsert or update `bead_runtime.grove_status = 'Running'`
5. set `bead_runtime.last_run_id`
6. append `event_log(RunStarted)`
7. commit

### Rule

A run row must exist before a Claude session is launched.
Never spawn the subprocess and only later try to create a run record.
If the process starts but the DB write fails, recovery becomes ambiguous.

## 24.4 `start session` transaction

Within one transaction:

1. create transcript path
2. insert `claude_sessions` row with `Starting`
3. optionally insert `prompt_materializations` row after prompt file is written
4. append `event_log(SessionStarted)`
5. commit

Only after commit may the backend spawn Claude.

## 24.5 `checkpoint rotation` transaction

When a session yields a checkpoint:

1. insert `checkpoints`
2. update `task_runs.last_checkpoint_id`
3. increment `task_runs.checkpoint_count`
4. mark session `Checkpointed`
5. set `bead_runtime.grove_status = 'Checkpointed'`
6. append `event_log(SessionCheckpointed)`
7. commit

After commit:

- node runner may decide whether to immediately continue with a fresh session
- any fresh session will become a new `claude_sessions` row with higher ordinal

### Why split commit from fresh spawn

Because checkpoint durability is the important irreversible event.
Spawning the next session is secondary and may happen later if the coordinator crashes.

## 24.6 `success handoff` transaction

When a session passes the exit gate and produces success:

1. upsert `handoffs`
2. mark session `Completed`
3. mark run `Succeeded`
4. set `bead_runtime.grove_status = 'Succeeded'`
5. release active reservations for this run/bead
6. append `event_log(SessionSucceeded)`
7. append `event_log(HandoffWritten)`
8. append `event_log(BrMirrorRequested)` for the future `br close`
9. commit

### Rule

Handoff persistence and grove runtime success must be in the same transaction.
It is invalid for grove to consider a bead successfully finished without a handoff.

### Important follow-up

Mirroring the result back to `br` with `br close` or `br comments add` happens **after** this transaction.
If that mirror step fails, grove should record `BrMirrorFailed` while preserving the successful local run record.

## 24.7 `failure terminalization` transaction

When the run should stop without immediate retry:

1. mark session terminal failure status
2. update run status to `Failed` or `WaitingToRetry`
3. write failure class/detail
4. release reservations if the run will not continue immediately
5. update `bead_runtime` accordingly
6. append `event_log(SessionFailed)`
7. commit

## 24.8 `reservation grant` transaction

Within one transaction:

1. query active overlapping reservations
2. if conflicting exclusive overlap exists, do not insert; return conflict set
3. otherwise insert reservation rows
4. append `event_log(ReservationGranted)`
5. commit

### Anti-race rule

Reservation conflict detection and insert must share the same transaction boundary.
If they are separated, parallel coordinators could overbook overlapping paths.

## 24.9 `recovery reconciliation` transaction pattern

Recovery should use many small, idempotent transactions rather than one giant startup mutation.

For each orphaned run/session pair:

1. inspect current durable facts
2. compute one deterministic repair action
3. apply that action atomically
4. append a recovery event
5. move on to next orphan

### Why

If recovery crashes halfway through, partial completed repairs are still valid and auditable.

## 24.10 Example SQL: list parent beads for a child bead

```sql
SELECT parent_id
FROM bead_dependencies
WHERE child_id = ?
ORDER BY parent_id;
```

## 24.11 Example SQL: active run for bead

```sql
SELECT *
FROM task_runs
WHERE bead_id = ?
  AND status IN ('Active', 'WaitingToRetry', 'Checkpointed')
ORDER BY attempt_no DESC
LIMIT 1;
```

## 24.12 Example SQL: active reservations overlapping candidate bead

The final overlap test is easier in Rust than raw SQL because glob semantics are application-specific.
The SQL query should therefore fetch candidate active reservations and let Rust make the final decision.

```sql
SELECT *
FROM reservations
WHERE released_at IS NULL
  AND expires_at > ?;
```

### Then in Rust

- normalize paths
- group by bead/run
- apply exact path + parent/child relation + symmetric glob logic

## 24.13 Example SQL: latest checkpoint for bead

```sql
SELECT *
FROM checkpoints
WHERE bead_id = ?
ORDER BY saved_at DESC
LIMIT 1;
```

## 24.14 Example SQL: latest handoffs for parent beads of a child bead

```sql
SELECT h.*
FROM bead_dependencies bd
JOIN handoffs h ON h.bead_id = bd.parent_id
WHERE bd.child_id = ?
ORDER BY h.completed_at ASC;
```

## 24.15 Example SQL: grove runtime status counts

```sql
SELECT grove_status, COUNT(*) AS count
FROM bead_runtime
GROUP BY grove_status
ORDER BY grove_status;
```

## 24.16 Example SQL: recent dispatch explanation

```sql
SELECT disposition, score_breakdown_json, blocking_reasons_json, created_at
FROM dispatch_decisions
WHERE bead_id = ?
ORDER BY created_at DESC
LIMIT 10;
```

---

## 25. CLI Contract and Command Behavior

The CLI should stay thin, but it still needs an explicit contract.
This section defines the expected UX surface for a **bead-backed** grove.

## 25.1 Global rules

- every command must support plain human-readable output
- diagnostic-heavy commands should later support `--json`
- errors should be concise and actionable
- no command should require the user to know internal file layout inside `.grove/`
- grove must not duplicate `br` or `bv` more than needed for convenience

## 25.2 CLI parser sketch

```rust
use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "grove")]
#[command(about = "Beads-backed Claude workflow orchestrator")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Init(InitArgs),
    Run(RunArgs),
    Status(StatusArgs),
    Inspect(InspectArgs),
    Retry(RetryArgs),
    Log(LogArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[arg(long)]
    pub force: bool,
}
```

### Bead ownership rule

Grove does **not** have bead creation or dependency commands.
Users manage beads directly with `br`. Grove only reads the `.beads` graph and mirrors results back after task completion.

## 25.3 `grove init`

### Responsibilities

- ensure workspace root is writable
- create `.grove/` directories
- initialize database and migrations
- write default `grove.toml` only if absent, unless `--force`
- verify `br`, `bv`, and `claude` are available
- print next-step guidance

### Expected output

```text
Initialized grove workspace.
- database: .grove/grove.db
- config: grove.toml
- transcripts: .grove/transcripts/
- checkpoints: .grove/checkpoints/

Validated tools:
- br
- bv
- claude

Next steps:
1. Create beads with `br` (user-managed)
2. grove run
```

## 25.4 `grove run`

### Responsibilities

- load validated config
- acquire leader lease
- run startup recovery
- sync ready beads from `br`
- optionally fetch scoring hints from `bv`
- enter polling/dispatch loop
- stream lightweight status changes
- exit with non-zero code if startup checks fail

### Important non-goal

`grove run` is not a shell-script supervisor.
It is the orchestrator process itself.

## 25.8 `grove status`

### Default sections

1. workspace summary
2. leader lease owner
3. bead counts by beads status and grove status
4. running beads
5. ready queue with score breakdown
6. checkpointed beads
7. failed beads
8. reservation conflicts

### Example shape

```text
Workspace: /repo
Leader: pid-1234@host (heartbeat 2s ago)

beads status counts:
- open: 8
- in_progress: 1
- closed: 5

Grove runtime counts:
- Ready: 3
- Running: 1
- Checkpointed: 1
- Failed: 0
- Succeeded: 5

Ready queue:
ID         Score  Why
bd-7f3a2c  137    P1 priority + long descendant chain
bd-e9b1d4  88     waiting age + no conflicts
bd-a1bc22  76     medium priority, one reservation penalty
```

## 25.9 `grove inspect <bead-id>`

### Sections

- bead header from cached `br show --json`
- dependency graph summary
- latest dispatch decisions
- run history
- latest session summary
- latest checkpoint if any
- latest handoff if succeeded
- `br` mirror actions attempted by grove
- relevant archive retrieval bundle if available
- selected playbook bullets if the bead is active

### Why this command matters

This is the main debugging entrypoint when a user asks:

- why did this bead not run?
- why was it retried?
- what context did it receive?
- what did the parent handoff say?
- did grove successfully mirror completion back to `br`?

## 25.10 `grove retry <bead-id>`

### Responsibilities

- only valid for `Failed` or manually paused `Checkpointed` runtime workflows
- clear retry backoff
- set grove runtime status back to `Ready` if `br` still reports the bead as ready
- append retry event

### Rule

Retry should create a new run.
It must not mutate historical run rows into pretending they never failed.

## 25.11 `grove log <bead-id>`

### Responsibilities

- show transcript lines from latest run/session
- optionally tail if the bead is active
- interleave parsed protocol events and raw text clearly

### Example format

```text
[session ses_01 stdout] planning change in crates/grove-orchestrator
[session ses_01 protocol] GROVE_CHECKPOINT progress="wired repos"
[session ses_02 stdout] continuing from checkpoint
[session ses_02 protocol] GROVE_RESULT summary="bead completed"
```

## 25.12 JSON mode contract

When JSON output is later added, shape it as structured records rather than free-form terminal dumps.

Example:

```json
{
  "workspace": "/repo",
  "leader": {
    "owner_label": "pid-1234@host",
    "expires_at": "2026-03-15T10:00:00Z"
  },
  "beads_counts": {
    "open": 8,
    "in_progress": 1,
    "closed": 5
  },
  "grove_counts": {
    "ready": 3,
    "running": 1,
    "checkpointed": 1,
    "failed": 0,
    "succeeded": 5
  }
}
```

---

## 26. Prompt Materialization and Claude Session Contract

This section intentionally separates grove-owned prompt logic from Claude CLI invocation details.
We should not hard-code guessed CLI behavior into the architecture spec.

## 26.1 Backend isolation rule

`grove-session::backend` is the only module allowed to know:

- how Claude is launched
- which argv form is used
- how stdout/stderr are captured
- how timeouts are enforced
- how child process handles are terminated

All other crates deal only with typed requests and typed outcomes.

## 26.2 Spawn request sketch

```rust
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub bead_id: BeadId,
    pub prompt_path: String,
    pub working_dir: String,
    pub timeout_secs: u64,
    pub environment: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct RunningChild {
    pub pid: u32,
    pub started_at: Timestamp,
}

#[async_trait::async_trait]
pub trait ClaudeBackend {
    async fn spawn(&self, req: SpawnRequest) -> anyhow::Result<RunningChild>;
    async fn wait(&self, child: RunningChild) -> anyhow::Result<BackendWaitResult>;
    async fn terminate(&self, pid: u32) -> anyhow::Result<()>;
}
```

## 26.3 Prompt materializer sketch

```rust
pub struct PromptMaterializer {
    paths: GrovePaths,
}

impl PromptMaterializer {
    pub fn write_prompt(&self, assembly: &PromptAssembly) -> anyhow::Result<String> {
        let path = self
            .paths
            .prompts_dir()
            .join(format!("{}-{}.md", assembly.bead_id.0, assembly.session_id.0));
        std::fs::write(&path, &assembly.prompt_text)?;
        Ok(path.display().to_string())
    }
}
```

### Rule

Prompts should be materialized to disk before session start so that:

- the exact content is inspectable
- prompt hash can be stored durably
- later recovery or audits can explain behavior

## 26.4 Prompt assembly order

The default segment order should be stable:

1. system-like grove instructions
2. bead definition
3. dependency handoffs
4. archive retrieval snippets
5. selected playbook bullets
6. checkpoint resume block when applicable
7. protocol contract block

### Why stable order matters

If prompt order changes unpredictably, debugging output quality and token pressure becomes much harder.

## 26.5 Prompt trimming rule

When prompt budget exceeds threshold, trim in this order:

1. drop lowest-scoring archive snippets
2. shrink archive excerpts before dropping dependency handoffs
3. reduce playbook bullets from weakest to strongest
4. never remove bead definition
5. never remove checkpoint summary when resuming
6. never remove protocol contract

## 26.6 Fresh bead prompt template

```text
You are executing one grove bead inside a larger orchestrated workflow.

Bead ID: {bead_id}
Title: {title}
Issue type: {issue_type}
Priority: {priority}
beads status: {br_status}
Grove runtime status: {grove_status}

Primary objective:
{description}

Workspace constraints:
- Work only inside the current repository.
- Prefer editing only files relevant to this bead.
- If you need to pause, emit a checkpoint using the grove protocol.
- If you finish, emit a final result using the grove protocol.

Declared path focus:
{declared_paths_block}
```

## 26.7 Parent handoff block template

```text
Parent bead handoffs:

{for each parent}
- Parent Bead: {parent_bead_id}
  Summary: {summary}
  Decisions:
  {decisions}
  Artifacts:
  {artifacts}
  Warnings:
  {warnings}
```

### Rule

Parent handoffs are summaries, not full transcript dumps.
The archive system exists to pull deeper context only when relevant.

## 26.8 Archive retrieval block template

```text
Relevant prior workspace context:

{for each retrieved snippet}
- Source Bead: {bead_id}
- File: {file_path}:{start_line}-{end_line}
- Why relevant: {reason}
- Excerpt:
```{language}
{snippet}
```
```

### Retrieval policy

- prefer exact file/path overlap
- then keyword overlap with current task title/description
- then recent successful conversations
- deduplicate near-identical snippets

## 26.9 Playbook rule block template

```text
Workspace playbook guidance:

{for each bullet}
- [{category}/{maturity}] {text}
```

### Rule quality constraints

Only inject bullets that are:

- not deprecated
- above score threshold
- directly relevant by scope or tags

## 26.10 Resume checkpoint block template

```text
Resume from prior checkpoint.

Progress so far:
{progress}

Recommended next step:
{next_step}

Open questions:
{open_questions}

Previously claimed paths:
{claimed_paths}

Checkpoint context JSON:
{payload_json}
```

## 26.11 Protocol contract block template

```text
At the end of the session, emit structured lines exactly in this format when applicable:

GROVE_RESULT: <one-line summary>
GROVE_ARTIFACTS: <JSON array of strings>
GROVE_LESSONS: <JSON array of strings>
GROVE_DECISIONS: <JSON array of strings>
GROVE_WARNINGS: <JSON array of strings>
GROVE_EXIT: true|false
GROVE_CHECKPOINT: <JSON object>

Rules:
- Emit GROVE_EXIT: true only when the task is actually complete.
- Emit GROVE_EXIT: false if more work is required.
- Emit GROVE_CHECKPOINT when the session should pause and continue later.
- Prefer valid JSON payloads for arrays and objects.
```

## 26.12 Parser state sketch

```rust
pub enum ParserLineKind {
    Protocol(ProtocolEvent),
    PlainStdout(String),
    PlainStderr(String),
}

pub struct ProtocolParser;

impl ProtocolParser {
    pub fn parse_line(&self, line: &str) -> Option<ProtocolEvent> {
        if let Some(value) = line.strip_prefix("GROVE_RESULT:") {
            return Some(ProtocolEvent::Result { summary: value.trim().to_string() });
        }
        if let Some(value) = line.strip_prefix("GROVE_EXIT:") {
            return Some(ProtocolEvent::Exit { value: value.trim() == "true" });
        }
        None
    }
}
```

### Implementation note

The real parser should support:

- JSON arrays for artifacts/lessons/decisions/warnings
- JSON object payload for checkpoints
- tolerant whitespace handling
- invalid-json capture as warning events rather than silent loss

## 26.13 Transcript JSONL format

Each transcript line should be durable and append-only.

Example:

```json
{"ts":"2026-03-15T10:00:00Z","kind":"session_started","session_id":"ses_01"}
{"ts":"2026-03-15T10:00:05Z","kind":"stdout","line":"Inspecting src/orchestrator/mod.rs"}
{"ts":"2026-03-15T10:00:07Z","kind":"protocol","event":{"type":"checkpoint","progress":"wired repos"}}
{"ts":"2026-03-15T10:00:20Z","kind":"session_ended","exit_code":0}
```

### Transcript rule

Transcript write order is the source of truth for replay and inspection.
Do not rewrite old events in place.

## 26.14 Exit evaluation rule

A session is successful only when:

1. there is no explicit `GROVE_EXIT: false`, and
2. either explicit `GROVE_EXIT: true` is present and completion indicators exceed threshold, or future policy relaxes this under specific config.

### Conservative default

For MVP, keep `require_explicit_exit = true`.

## 26.15 Failure classification matrix

| Signal | Classification |
|---|---|
| process timeout reached | `Timeout` |
| backend detects rate limit text/payload | `RateLimit` |
| permission denied markers detected | `PermissionDenied` |
| process exits unexpectedly without valid success/checkpoint | `Crash` or `UnknownFailure` |
| repeated identical failure + breaker threshold | `NoProgress` or `SameErrorLoop` |

## 26.16 Context pressure estimator sketch

```rust
pub struct ContextPressure {
    pub prompt_chars: usize,
    pub transcript_chars: usize,
    pub checkpoint_chars: usize,
    pub estimated_total: usize,
    pub pressure_ratio: f32,
    pub should_checkpoint_soon: bool,
}
```

### Heuristic input sources

- prompt materialized bytes
- cumulative stdout/stderr bytes
- structured checkpoint payload size
- configurable model budget estimate

### Rule

This is advisory, not authoritative.
It exists to encourage early checkpointing before quality collapses.

---

## 27. State Transitions and Recovery Tables

This section removes ambiguity around what transitions are legal.

## 27.1 Task state transitions

| From | To | Allowed | Reason |
|---|---|---|---|
| `Pending` | `Ready` | yes | readiness recompute unblocked it |
| `Pending` | `Running` | yes | direct dispatch path may mark it running after run creation |
| `Ready` | `Running` | yes | task dispatched |
| `Running` | `Checkpointed` | yes | latest session produced checkpoint |
| `Running` | `Succeeded` | yes | handoff written successfully |
| `Running` | `Failed` | yes | terminal failure without immediate retry |
| `Checkpointed` | `Running` | yes | resumed through new session |
| `Checkpointed` | `Ready` | yes | recovery or manual retry reopened it |
| `Failed` | `Ready` | yes | explicit retry or retry policy reopened it |
| `Succeeded` | any non-terminal | no | create a new child task instead of mutating history |

## 27.2 Run state transitions

| From | To | Allowed |
|---|---|---|
| `Active` | `Checkpointed` | yes |
| `Active` | `WaitingToRetry` | yes |
| `Active` | `Succeeded` | yes |
| `Active` | `Failed` | yes |
| `WaitingToRetry` | `Active` | yes |
| `Checkpointed` | `Active` | yes |
| `Failed` | any | no, create new run |
| `Succeeded` | any | no |

## 27.3 Session state transitions

| From | To | Allowed |
|---|---|---|
| `Starting` | `Running` | yes |
| `Running` | `Completed` | yes |
| `Running` | `Checkpointed` | yes |
| `Running` | `TimedOut` | yes |
| `Running` | `RateLimited` | yes |
| `Running` | `PermissionDenied` | yes |
| `Running` | `Crashed` | yes |
| `Running` | `UnknownFailure` | yes |
| terminal session state | any | no |

## 27.4 Breaker state transitions

| From | To | Trigger |
|---|---|---|
| `Closed` | `HalfOpen` | soft threshold reached or cooldown trial starts |
| `HalfOpen` | `Open` | immediate repeated failure |
| `HalfOpen` | `Closed` | progress and stable success observed |
| `Closed` | `Open` | hard threshold reached |
| `Open` | `HalfOpen` | cooldown expired |

## 27.5 Recovery decision matrix

| Durable facts | Action |
|---|---|
| active run + running session + process alive | leave intact |
| active run + running session + process dead + checkpoint exists | mark session failed, run checkpointed/ready depending policy |
| active run + running session + process dead + no checkpoint + retry budget remains | mark session crashed, run waiting-to-retry |
| active run + no active session + latest checkpoint exists | task becomes `Checkpointed` or `Ready` |
| task marked `Running` + no active run | repair task status from latest run facts |
| expired lease + no owner process | takeover allowed |

## 27.6 Reservation recovery matrix

| Reservation facts | Action |
|---|---|
| released_at set | leave it |
| expires_at in future and owning run still active | leave it |
| expires_at in past | expire and append event |
| owning run terminal and reservation unreleased | release it |
| owning bead succeeded and active reservation remains | release it as integrity repair |

## 27.7 Handoff integrity matrix

| Grove runtime status | Handoff present? | Valid? |
|---|---|---|
| `Succeeded` | yes | valid |
| `Succeeded` | no | invalid, must repair or fail integrity check |
| `Running` | yes | suspicious but possible if finalization partially failed |
| `Failed` | yes | suspicious unless marked as partial output in future extension |

## 27.8 Prompt integrity matrix

| Session exists | Prompt materialization exists | Meaning |
|---|---|---|
| yes | yes | normal |
| yes | no | incomplete diagnostics; warn |
| no | yes | orphan prompt file; can remain for audit |

---

## 28. Sample Durable Artifacts

This appendix defines example on-disk payloads so implementation can align serialization from the start.

## 28.1 Example checkpoint JSON file

```json
{
  "id": "chk_01HXYZ",
  "bead_id": "bd-e9b1d4",
  "run_id": "run_01HXYZ",
  "session_id": "ses_01HXYZ",
  "progress": "Implemented br sync and bead-cache refresh; completion mirror still pending.",
  "next_step": "Call br close for the completed bead and verify local runtime transitions stay consistent.",
  "payload": {
    "open_questions": [
      "Should grove comment before or after calling br close on success?"
    ],
    "claimed_paths": [
      "crates/grove-br/src/client.rs",
      "crates/grove-orchestrator/src/node_runner.rs"
    ],
    "confidence": 0.81
  },
  "saved_at": "2026-03-15T10:15:00Z",
  "resume_generation": 2
}
```

## 28.2 Example handoff JSON projection

```json
{
  "bead_id": "bd-e9b1d4",
  "run_id": "run_01HXYZ",
  "summary": "Implemented bead completion mirroring into br and persisted grove runtime success state.",
  "artifacts": [
    "crates/grove-br/src/client.rs",
    "crates/grove-orchestrator/src/node_runner.rs",
    "crates/grove-db/src/run_repo.rs"
  ],
  "lessons": [
    "Keep grove runtime state private even when mirroring completion back into br."
  ],
  "decisions": [
    "br remains authoritative for issue closure; grove only records the mirror attempt and result."
  ],
  "warnings": [
    "If br close fails after handoff persistence, grove must mark the mirror attempt failed without losing the successful run record."
  ],
  "completed_at": "2026-03-15T10:22:00Z"
}
```

## 28.3 Example dispatch decision payload

```json
{
  "bead_id": "bd-7f3a2c",
  "tick_id": "tick_01",
  "disposition": "blocked_by_reservation",
  "score_breakdown": {
    "base_priority": 50,
    "critical_path_bonus": 12,
    "waiting_age_bonus": 8,
    "retry_penalty": 0,
    "reservation_penalty": 100,
    "breaker_penalty": 0,
    "final_score": -30
  },
  "blocking_reasons": [
    "exclusive overlap with bd-e9b1d4 on crates/grove-session/**"
  ],
  "competing_bead_ids": [
    "bd-e9b1d4"
  ]
}
```

## 28.4 Example transcript JSONL sequence

```json
{"ts":"2026-03-15T10:00:00Z","kind":"session_started","session_id":"ses_01"}
{"ts":"2026-03-15T10:00:01Z","kind":"stdout","line":"Reading crates/grove-kernel/src/services/task_service.rs"}
{"ts":"2026-03-15T10:00:05Z","kind":"protocol","event":{"type":"decision","items":["Kernel will own readiness transitions"]}}
{"ts":"2026-03-15T10:00:06Z","kind":"protocol","event":{"type":"checkpoint","payload":{"progress":"task repo done","next_step":"wire dep repo"}}}
{"ts":"2026-03-15T10:00:07Z","kind":"session_ended","exit_code":0}
```

## 28.5 Example selected playbook bullets bundle

```json
[
  {
    "id": "bul_01",
    "category": "architecture",
    "text": "Prefer cycle detection in Rust service code for MVP instead of recursive SQL.",
    "maturity": "active",
    "effective_score": 0.92
  },
  {
    "id": "bul_02",
    "category": "recovery",
    "text": "Persist checkpoint durability before attempting to spawn the next session.",
    "maturity": "active",
    "effective_score": 0.88
  }
]
```

## 28.6 Example `grove.toml`

```toml
[workspace]
root = "."

[database]
path = ".grove/grove.db"
busy_timeout_ms = 5000

[orchestrator]
max_concurrency = 2
poll_interval_ms = 1500
lease_ttl_ms = 10000
lease_heartbeat_ms = 3000

[retry]
max_attempts = 3
timeout_backoff_secs = 15
rate_limit_backoff_secs = 60
crash_backoff_secs = 10
no_progress_backoff_secs = 30
permission_denied_requires_manual_retry = true

[context]
soft_ratio = 0.72
hard_ratio = 0.85

[prompt]
max_archive_snippets = 6
max_playbook_bullets = 12
```

---

## 29. Phase-by-Phase Coding Micro-Plan

The earlier implementation sequence names phases.
This section breaks the phases into concrete file-level steps.

## 29.1 Phase 1 micro-steps

### Step 1

Create root workspace files:

- `Cargo.toml`
- `rust-toolchain.toml`
- `grove.toml`
- `.gitignore` updates if needed

### Step 2

Create `grove-types` with:

- IDs
- task model
- run model
- session model
- checkpoint model
- handoff model
- reservation model
- event model

### Step 3

Create `grove-config` with:

- config structs
- default values
- path resolver
- TOML loader
- validator

### Step 4

Create `grove-db` with:

- connection wrapper
- migration runner
- `0001_init.sql`
- minimal repositories for tasks, dependencies, runs, sessions, checkpoints, handoffs

### Step 5

Create `grove-kernel` with:

- task service
- dependency service
- cycle detection
- readiness recompute
- inspect/status query helpers

### Step 6

Create `grove-cli` commands:

- `init`
- `status`
- `inspect`
- `log`
- `retry`

### Phase 1 done means

- grove can discover and cache beads state
- ready vs blocked comes from `br`
- graph insight can be augmented from `bv`
- no Claude runtime integration exists yet

## 29.2 Phase 2 micro-steps

### Step 1

Create `grove-session::protocol`:

- protocol event enum
- string prefixes
- JSON parsing helpers

### Step 2

Create `grove-session::parser`:

- line parser
- invalid protocol capture behavior
- structured event conversion

### Step 3

Create `grove-session::transcript`:

- JSONL writer
- append semantics
- flush behavior

### Step 4

Create `grove-session::analysis`:

- progress signal detection
- completion indicator counting
- exit false override
n
### Step 5

Create `grove-session::exit_policy` and `classifier`.

### Step 6

Create `grove-session::backend` abstraction and a single Claude CLI backend implementation.

### Step 7

Create `grove-session::runner` joining:

- prompt materialization
- backend spawn/wait
- transcript capture
- parsing
- analysis
- classification

### Phase 2 done means

- one standalone task prompt can be executed
- session outcome is classified durably
- transcript JSONL exists on disk
- success requires explicit exit gate satisfaction

## 29.3 Phase 3 micro-steps

### Step 1

Create orchestrator queue and ready evaluation.

### Step 2

Create node runner with:

- open run
- create session
- spawn Claude
- store outcome
- checkpoint rotation
- success finalization
- failure handling

### Step 3

Create recovery reconciler.

### Step 4

Create leader lease acquisition and heartbeat.

### Step 5

Create `grove run` loop.

### Phase 3 done means

- sequential DAG execution works end-to-end
- child tasks unblock after parent handoff persists
- checkpoints can lead to fresh sessions
- restart recovery preserves consistency

## 29.4 Phase 4 micro-steps

### Step 1

Create archive data model and DB repo.

### Step 2

Create transcript ingest normalization.

### Step 3

Create FTS insert/sync behavior.

### Step 4

Create retrieval bundle builder for prompt assembly.

### Phase 4 done means

- transcript-derived knowledge is searchable locally
- new tasks can receive prior snippets
- no external archive/search CLI is involved

## 29.5 Phase 5 micro-steps

### Step 1

Create playbook bullet types and repos.

### Step 2

Create lesson ingestion path from handoffs and explicit `GROVE_LESSONS`.

### Step 3

Create scoring and evidence gate.

### Step 4

Create selector injection into prompt building.

### Phase 5 done means

- useful repeated rules are promoted
- weak/noisy rules stay non-authoritative
- memory remains native and compact

## 29.6 Phase 6 micro-steps

### Step 1

Create reservation manager.

### Step 2

Create conflict-aware scheduler.

### Step 3

Add bounded concurrency to coordinator.

### Step 4

Expose dispatch explanations in status/inspect.

### Phase 6 done means

- safe parallelism works
- overlapping paths do not run together unsafely
- stale reservations recover after crash

## 29.7 Phase 7 micro-steps

### Step 1

Create diary generation from run outcome.

### Step 2

Create exact and approximate duplicate reinforcement.

### Step 3

Create harmful-rule inversion.

### Step 4

Refine selector and pruning.

### Phase 7 done means

- playbook stays compact
- harmful stale rules do not keep poisoning prompts
- evidence remains explainable

---

## 30. Final Implementation Discipline Rules

These rules are here to prevent the plan from drifting back toward wrapper architecture during coding.

### Rule 1

If a design choice can be expressed as either:

- “store durable typed state in SQLite + Rust structs”, or
- “parse shell output from another CLI later”,

choose the durable typed state.

### Rule 2

If a debugging need can be solved by either:

- adding a typed event or table, or
- scraping logs ad hoc from many places,

prefer the typed event/table.

### Rule 3

If a useful pattern is traditionally implemented via Bash, tmux, YAML, or CLI shells, port the idea into native Rust and discard the shell.

### Rule 4

Keep `grove-cli` thin.
If business logic starts accumulating there, move it into kernel/session/orchestrator crates.

### Rule 5

Never mark a task `Succeeded` without a handoff.
Never mark a session `Completed` without passing the exit gate.
Never create a fresh session from a checkpoint until that checkpoint is durable.

### Rule 6

Prefer recovery that is:

- idempotent
- visible in event log
- conservative
- easy to reason about after a crash

### Rule 7

All design patterns in Section 1 are grove-native implementations.
They are not runtime dependencies on external tools.
They are not wrappers around other products.
They are not required installs for the user.

### Rule 8

The user-facing story must stay simple:

- install grove
- configure Claude runtime
- create beads
- run grove
- inspect status and logs

Anything that forces the user to understand external orchestration or memory tool internals is a design regression.
Needing to understand the normal `br` / `bv` workflow is acceptable because that is now part of the intended product contract.

---

## 31. Safety Guard and Destructive Command Detection

Grove orchestrates autonomous Claude sessions that may run for extended periods without human supervision. A safety guard layer prevents catastrophic damage from destructive operations that Claude might attempt during execution.

## 31.1 Threat Model

Autonomous agent sessions can produce destructive commands in several ways:

- Claude misinterprets a task and attempts to delete production data
- A playbook bullet or archive snippet injects a harmful pattern
- Context window pressure causes Claude to take shortcuts (e.g., `rm -rf` instead of targeted cleanup)
- A checkpoint resume misaligns the agent's understanding of workspace state
- Claude attempts force-pushing to protected branches or resetting git history

Grove does not block Claude's tool use at the process level (Claude controls its own tool calls). However, grove can:

1. **Pre-session**: Inject safety rules into prompts
2. **Post-session**: Detect destructive patterns in transcripts and flag them
3. **Cross-session**: Learn from past destructive incidents via the playbook engine

## 31.2 Destructive Pattern Registry

The safety guard maintains a registry of known destructive command patterns.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestructivePattern {
    pub id: String,
    pub category: DestructiveCategory,
    pub pattern: String,
    pub pattern_type: PatternType,
    pub severity: PatternSeverity,
    pub description: String,
    pub mitigation: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DestructiveCategory {
    FileSystem,
    Database,
    Git,
    Infrastructure,
    Network,
    Credentials,
    Build,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PatternType {
    Regex,
    Literal,
    GlobPath,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum PatternSeverity {
    Warning,
    Dangerous,
    Critical,
}
```

### Built-in patterns

**FileSystem — Critical**:

```rust
const FS_CRITICAL_PATTERNS: &[(&str, &str)] = &[
    (r"^rm\s+(-[rf]+\s+)+/(etc|usr|var|boot|home|root|bin|sbin|lib)", "Recursive delete of system directories"),
    (r"rm\s+-[rf]*\s+/\s", "Delete root filesystem"),
    (r"rm\s+-[rf]*\s+\.\s*$", "Delete current directory recursively"),
    (r"rm\s+-[rf]*\s+\*\s*$", "Delete all files in directory"),
    (r"mkfs\.", "Format filesystem"),
    (r"dd\s+.*of=/dev/[sh]d", "Direct disk write"),
    (r">\s*/dev/[sh]d", "Redirect to raw disk device"),
    (r"chmod\s+-R\s+777\s+/", "World-writable root filesystem"),
    (r"chown\s+-R\s+.*\s+/[^/]", "Recursive chown on system directory"),
];
```

**Database — Critical**:

```rust
const DB_CRITICAL_PATTERNS: &[(&str, &str)] = &[
    (r"DROP\s+DATABASE", "Drop entire database"),
    (r"DROP\s+SCHEMA\s+.*CASCADE", "Drop schema with cascade"),
    (r"TRUNCATE\s+TABLE", "Truncate table data"),
    (r"DELETE\s+FROM\s+\w+\s*;?\s*$", "Delete all rows without WHERE clause"),
    (r"UPDATE\s+\w+\s+SET\s+.*(?!WHERE)", "Update all rows without WHERE clause"),
    (r"DROP\s+TABLE\s+IF\s+EXISTS\s+\w+\s*;\s*DROP\s+TABLE", "Cascading table drops"),
];
```

**Git — Critical**:

```rust
const GIT_CRITICAL_PATTERNS: &[(&str, &str)] = &[
    (r"git\s+push\s+.*--force(?!\s+--with-lease)", "Force push without lease"),
    (r"git\s+push\s+.*-f\s", "Force push shorthand"),
    (r"git\s+reset\s+--hard\s+HEAD~", "Hard reset to ancestor"),
    (r"git\s+reset\s+--hard\s+origin/", "Hard reset to remote"),
    (r"git\s+clean\s+-[dxf]*d[xf]*", "Clean untracked directories"),
    (r"git\s+checkout\s+--\s+\.", "Discard all working changes"),
    (r"git\s+stash\s+drop\s+stash@\{0\}", "Drop most recent stash"),
    (r"git\s+branch\s+-D\s+(main|master|develop|release)", "Delete protected branch"),
    (r"git\s+push\s+origin\s+--delete\s+(main|master)", "Delete remote main branch"),
    (r"git\s+rebase\s+.*--force", "Force rebase"),
];
```

**Infrastructure — Critical**:

```rust
const INFRA_CRITICAL_PATTERNS: &[(&str, &str)] = &[
    (r"terraform\s+destroy", "Terraform destroy"),
    (r"kubectl\s+delete\s+(node|namespace|pv|pvc|crd)", "Delete Kubernetes critical resources"),
    (r"kubectl\s+delete\s+--all", "Delete all Kubernetes resources"),
    (r"docker\s+system\s+prune\s+-a", "Prune all Docker resources"),
    (r"docker\s+rm\s+-f\s+\$\(docker\s+ps", "Force remove all containers"),
    (r"systemctl\s+(stop|disable)\s+(docker|kubelet|sshd|networking)", "Stop critical services"),
    (r"helm\s+uninstall\s+.*--no-hooks", "Helm uninstall without hooks"),
    (r"aws\s+s3\s+rm\s+s3://.*--recursive", "Recursive S3 delete"),
    (r"gcloud\s+.*delete\s+.*--quiet", "Silent GCP resource delete"),
];
```

**Credentials — Critical**:

```rust
const CRED_CRITICAL_PATTERNS: &[(&str, &str)] = &[
    (r"cat\s+.*\.env", "Read environment secrets"),
    (r"echo\s+.*password", "Echo password to stdout"),
    (r"curl\s+.*-d\s+.*token", "Send token via curl"),
    (r"printenv\s+(.*KEY|.*SECRET|.*TOKEN|.*PASSWORD)", "Print secret environment variables"),
    (r"git\s+config\s+.*credential", "Modify git credentials"),
    (r"ssh-keygen\s+.*-y", "Extract SSH public key from private key"),
];
```

**Git — Warning (legitimate but risky)**:

```rust
const GIT_WARNING_PATTERNS: &[(&str, &str)] = &[
    (r"git\s+push\s+--force-with-lease", "Force push with lease (safer but still overwrites)"),
    (r"git\s+rebase\s+-i", "Interactive rebase (rewrites history)"),
    (r"git\s+commit\s+--amend", "Amend commit (rewrites history)"),
    (r"git\s+filter-branch", "Filter branch (rewrites history)"),
    (r"git\s+cherry-pick\s+--abort", "Abort cherry-pick"),
];
```

## 31.3 Transcript Scanning Engine

After each session completes, grove scans the transcript for destructive patterns.

```rust
pub struct TranscriptScanResult {
    pub session_id: SessionId,
    pub matches: Vec<PatternMatch>,
    pub severity_summary: SeveritySummary,
    pub scan_duration_ms: u64,
}

pub struct PatternMatch {
    pub pattern_id: String,
    pub category: DestructiveCategory,
    pub severity: PatternSeverity,
    pub matched_text: String,
    pub transcript_line: usize,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
    pub timestamp: Option<DateTime<Utc>>,
}

pub struct SeveritySummary {
    pub critical_count: u32,
    pub dangerous_count: u32,
    pub warning_count: u32,
    pub highest_severity: PatternSeverity,
}
```

### Scanning algorithm

```rust
fn scan_transcript(
    transcript_path: &Utf8Path,
    registry: &PatternRegistry,
) -> anyhow::Result<TranscriptScanResult> {
    let mut matches = Vec::new();
    let lines: Vec<String> = read_transcript_lines(transcript_path)?;
    let compiled = registry.compile_patterns()?;

    for (line_idx, line) in lines.iter().enumerate() {
        if let Some(content) = extract_stdout_content(line) {
            for pattern in &compiled {
                if pattern.regex.is_match(&content) {
                    matches.push(PatternMatch {
                        pattern_id: pattern.id.clone(),
                        category: pattern.category,
                        severity: pattern.severity,
                        matched_text: content.clone(),
                        transcript_line: line_idx,
                        context_before: lines[line_idx.saturating_sub(3)..line_idx].to_vec(),
                        context_after: lines[line_idx + 1..lines.len().min(line_idx + 4)].to_vec(),
                        timestamp: extract_timestamp(line),
                    });
                }
            }
        }
    }

    let severity_summary = SeveritySummary::from_matches(&matches);
    Ok(TranscriptScanResult {
        session_id: extract_session_id(transcript_path),
        matches,
        severity_summary,
        scan_duration_ms: 0,
    })
}
```

### Context window for matches

Each match captures 3 lines before and after for human review. This context helps determine whether a destructive command was actually executed or just discussed.

## 31.4 Incident Records

When destructive patterns are detected, grove persists incident records for auditability.

```sql
CREATE TABLE safety_incidents (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    bead_id TEXT NOT NULL,
    pattern_id TEXT NOT NULL,
    category TEXT NOT NULL,
    severity TEXT NOT NULL,
    matched_text TEXT NOT NULL,
    transcript_line INTEGER NOT NULL,
    context_json TEXT NOT NULL,
    disposition TEXT NOT NULL DEFAULT 'detected',
    reviewed_at TEXT,
    reviewed_by TEXT,
    review_note TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES claude_sessions(id) ON DELETE CASCADE,
    FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE
);

CREATE INDEX idx_safety_incidents_session ON safety_incidents(session_id);
CREATE INDEX idx_safety_incidents_severity ON safety_incidents(severity, created_at DESC);
CREATE INDEX idx_safety_incidents_bead ON safety_incidents(bead_id);
CREATE INDEX idx_safety_incidents_disposition ON safety_incidents(disposition);
```

### Disposition values

- `detected` — pattern matched, awaiting review
- `confirmed` — human confirmed destructive action occurred
- `false_positive` — pattern matched but action was benign (e.g., discussed but not executed)
- `mitigated` — destructive action occurred but damage was contained
- `escalated` — forwarded to external incident response

## 31.5 Prompt Safety Injection

For every Claude session, grove injects a safety preamble into the prompt.

```text
[GROVE SAFETY RULES]
You are working in an automated orchestration environment. Exercise extreme caution with:

1. NEVER execute destructive filesystem commands (rm -rf, mkfs, dd to disk devices)
2. NEVER drop databases or truncate tables without explicit task authorization
3. NEVER force-push to main/master/develop branches
4. NEVER expose credentials, tokens, or secrets in output
5. NEVER delete Kubernetes nodes, namespaces, or persistent volumes
6. NEVER run terraform destroy without explicit confirmation in the task description

If your task requires any of the above, emit:
  GROVE_WARNINGS: ["Destructive operation requested: <description>"]
  GROVE_EXIT: false

Wait for the next session with explicit authorization before proceeding.

Prefer safe alternatives:
- git push --force-with-lease instead of --force
- Soft deletes or renames instead of rm -rf
- Database backups before schema changes
- Dry-run flags for infrastructure commands
```

### Safety injection rules

- Safety preamble is **always** injected; it cannot be disabled via config
- Safety preamble is a `required = true` prompt segment (see Section 11.3)
- It is inserted between the system instructions and the bead definition
- Token cost is minimal (~200 tokens) relative to typical prompt budgets

## 31.6 Apology Pattern Detection

When Claude encounters its own mistakes during a session, the transcript often contains apology language. Grove uses this as a secondary signal for incident investigation.

```rust
const APOLOGY_KEYWORDS: &[&str] = &[
    "sorry",
    "apologies",
    "my mistake",
    "I made an error",
    "accidentally",
    "unintended",
    "shouldn't have",
    "wrong file",
    "wrong directory",
    "overwritten",
    "lost work",
    "destroyed",
    "wiped",
    "deleted by mistake",
    "corrupted",
    "broke the build",
    "regression introduced",
];
```

### Apology detection rules

- Scan agent-role messages only (not user/system/tool messages)
- Count apology keyword hits per session
- If `apology_count >= 3` in a single session, flag the session for review
- Correlate apology timestamps with nearby destructive pattern matches
- Record apology count in the session's `metadata_json` field

## 31.7 Learning from Incidents

When a safety incident is confirmed, grove feeds it back into the playbook engine:

1. Create a new `AntiPattern` bullet with the incident pattern description
2. Set maturity to `Candidate` with one `Harmful` feedback event
3. If the same pattern recurs across multiple beads, promote to `Established`
4. Inject the anti-pattern into future prompts as a warning

```rust
fn incident_to_bullet(incident: &SafetyIncident) -> PlaybookDelta {
    PlaybookDelta::Add {
        bullet: PlaybookBullet {
            scope: BulletScope::Global,
            kind: BulletKind::Avoid,
            bullet_type: BulletType::AntiPattern,
            text: format!(
                "Avoid: {} — detected in bead {} session {}",
                incident.matched_text, incident.bead_id.0, incident.session_id.0
            ),
            state: BulletState::Active,
            maturity: BulletMaturity::Candidate,
            source: BulletSource::OutcomeDeduced,
            tags: vec!["safety".to_string(), incident.category.to_string()],
            ..Default::default()
        },
    }
}
```

## 31.8 Safety Guard Module Mapping

| Concept | Grove module |
|---------|-------------|
| DestructivePattern registry | `grove-session::safety::registry` |
| Compiled pattern cache | `grove-session::safety::compiled` |
| Transcript scanner | `grove-session::safety::scanner` |
| Incident persistence | `grove-db::safety_repo` |
| Prompt safety injection | `grove-session::prompt_builder` |
| Apology detection | `grove-session::safety::apology` |
| Incident-to-playbook | `grove-memory::playbook::safety_feedback` |

## 31.9 Safety Configuration

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyConfig {
    pub scan_transcripts: bool,
    pub inject_safety_preamble: bool,
    pub custom_patterns: Vec<DestructivePattern>,
    pub disabled_builtin_patterns: Vec<String>,
    pub apology_threshold: u32,
    pub block_on_critical: bool,
    pub incident_retention_days: u32,
}
```

### Defaults

```toml
[safety]
scan_transcripts = true
inject_safety_preamble = true
custom_patterns = []
disabled_builtin_patterns = []
apology_threshold = 3
block_on_critical = false
incident_retention_days = 90
```

### `block_on_critical`

When set to `true`, if a critical pattern is detected in a running session's stdout, grove will:

1. Log the incident immediately
2. Set circuit breaker to `Open`
3. Terminate the session
4. Mark the run as `Failed` with `FailureClass::SafetyViolation`
5. Require explicit `grove retry --force` to resume

This is disabled by default because it requires real-time stdout scanning, which adds latency. Phase 1 uses post-session scanning only.

## 31.10 ReDoS Protection for Custom Patterns

User-provided regex patterns must be validated against ReDoS (Regular expression Denial of Service) attacks.

```rust
fn validate_regex_safety(pattern: &str) -> Result<(), PatternValidationError> {
    if pattern.len() > 256 {
        return Err(PatternValidationError::TooLong(pattern.len()));
    }
    if pattern.contains("(.*)") && pattern.matches("(.*)").count() >= 2 {
        return Err(PatternValidationError::PotentialReDoS("nested .* groups".into()));
    }
    if pattern.contains("(.*)*") || pattern.contains("(.+)+") {
        return Err(PatternValidationError::PotentialReDoS("catastrophic backtracking".into()));
    }
    let timeout = Duration::from_millis(100);
    let test_input = "a".repeat(1000);
    let re = regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| PatternValidationError::InvalidRegex(e.to_string()))?;
    let start = Instant::now();
    let _ = re.is_match(&test_input);
    if start.elapsed() > timeout {
        return Err(PatternValidationError::TooSlow(start.elapsed()));
    }
    Ok(())
}

#[derive(Debug)]
pub enum PatternValidationError {
    TooLong(usize),
    PotentialReDoS(String),
    InvalidRegex(String),
    TooSlow(Duration),
}
```

---

## 32. Deprecated Pattern Detection and Active Monitoring

Beyond safety guards for destructive commands, grove tracks **deprecated coding patterns** — idioms, APIs, or approaches that were once acceptable but should no longer be used. This feeds into both prompt injection (warn Claude) and post-session validation.

## 32.1 Deprecated Pattern Model

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeprecatedPattern {
    pub id: String,
    pub pattern: String,
    pub pattern_type: PatternType,
    pub scope: BulletScope,
    pub scope_key: Option<String>,
    pub description: String,
    pub replacement: Option<String>,
    pub deprecated_at: DateTime<Utc>,
    pub reason: String,
    pub source_bullet_id: Option<BulletId>,
    pub active: bool,
}
```

### SQL schema

```sql
CREATE TABLE deprecated_patterns (
    id TEXT PRIMARY KEY,
    pattern TEXT NOT NULL,
    pattern_type TEXT NOT NULL,
    scope TEXT NOT NULL,
    scope_key TEXT,
    description TEXT NOT NULL,
    replacement TEXT,
    deprecated_at TEXT NOT NULL,
    reason TEXT NOT NULL,
    source_bullet_id TEXT,
    active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    FOREIGN KEY (source_bullet_id) REFERENCES playbook_bullets(id) ON DELETE SET NULL
);

CREATE INDEX idx_deprecated_patterns_scope ON deprecated_patterns(scope, scope_key);
CREATE INDEX idx_deprecated_patterns_active ON deprecated_patterns(active);
```

## 32.2 Pattern Sources

Deprecated patterns can come from:

1. **Explicit user configuration** — `grove.toml` or dedicated pattern file
2. **Playbook demotion** — when a bullet is deprecated via the curation pipeline, its content becomes a deprecated pattern automatically
3. **Anti-pattern inversion** — when a rule is inverted to an anti-pattern with strong evidence
4. **Manual CLI** — `grove pattern add --deprecated "old_api_call" --replacement "new_api_call"`

### Automatic extraction from playbook deprecation

```rust
fn bullet_to_deprecated_pattern(bullet: &PlaybookBullet) -> Option<DeprecatedPattern> {
    if bullet.bullet_type != BulletType::AntiPattern || !bullet.deprecated {
        return None;
    }
    let pattern = extract_code_pattern(&bullet.text)?;
    Some(DeprecatedPattern {
        id: format!("dp-{}", &bullet.id.0),
        pattern,
        pattern_type: PatternType::Literal,
        scope: bullet.scope,
        scope_key: bullet.scope_key.clone(),
        description: bullet.text.clone(),
        replacement: bullet.tags.iter()
            .find(|t| t.starts_with("replacement:"))
            .map(|t| t.strip_prefix("replacement:").unwrap_or("").to_string()),
        deprecated_at: bullet.updated_at,
        reason: bullet.deprecation_reason.clone().unwrap_or_default(),
        source_bullet_id: Some(bullet.id.clone()),
        active: true,
    })
}
```

## 32.3 Transcript Pattern Checker

After session completion, check transcript output against deprecated patterns:

```rust
pub struct DeprecatedPatternMatch {
    pub pattern_id: String,
    pub matched_text: String,
    pub file_path: Option<String>,
    pub line_number: Option<u32>,
    pub replacement: Option<String>,
    pub transcript_line: usize,
}

pub struct DeprecatedPatternScanResult {
    pub session_id: SessionId,
    pub matches: Vec<DeprecatedPatternMatch>,
    pub unique_patterns_hit: usize,
}
```

### Post-session hook

```rust
async fn post_session_deprecated_check(
    session_id: &SessionId,
    transcript_path: &Utf8Path,
    patterns: &[DeprecatedPattern],
) -> anyhow::Result<DeprecatedPatternScanResult> {
    let content = read_transcript_content(transcript_path)?;
    let mut matches = Vec::new();
    let mut seen_patterns = HashSet::new();

    for pattern in patterns.iter().filter(|p| p.active) {
        let re = compile_pattern(pattern)?;
        for (line_idx, line) in content.lines().enumerate() {
            if re.is_match(line) {
                seen_patterns.insert(&pattern.id);
                matches.push(DeprecatedPatternMatch {
                    pattern_id: pattern.id.clone(),
                    matched_text: line.to_string(),
                    file_path: extract_file_reference(line),
                    line_number: extract_line_number(line),
                    replacement: pattern.replacement.clone(),
                    transcript_line: line_idx,
                });
            }
        }
    }

    Ok(DeprecatedPatternScanResult {
        session_id: session_id.clone(),
        matches,
        unique_patterns_hit: seen_patterns.len(),
    })
}
```

## 32.4 Prompt Injection of Deprecated Warnings

When building prompts for a bead, inject active deprecated patterns relevant to the bead's scope:

```text
[DEPRECATED PATTERNS — Do NOT use these]

- `old_database_query()` → Use `new_database_query()` instead
  Reason: Performance regression confirmed in 3 sessions

- `sync_all_files()` → Use `sync_changed_files()` instead
  Reason: Causes unnecessary I/O, deprecated since 2026-03-10
```

### Injection budget

- Maximum 8 deprecated pattern warnings per prompt
- Prefer patterns matching the bead's declared paths or scope
- Sort by recency of deprecation
- Each warning costs approximately 30 tokens

## 32.5 Module Mapping

| Concept | Grove module |
|---------|-------------|
| Deprecated pattern model | `grove-memory::playbook::deprecated` |
| Pattern validation + compilation | `grove-session::safety::pattern_compiler` |
| Transcript pattern checker | `grove-session::safety::deprecated_checker` |
| Prompt deprecated injection | `grove-session::prompt_builder` |
| Auto-extraction from playbook | `grove-memory::playbook::curate` |

---

## 33. Context Relevance Scoring for Prompt Injection

This section defines the exact algorithms for scoring and selecting content to inject into Claude prompts. The prompt builder must balance relevance, recency, diversity, and budget.

## 33.1 Archive Snippet Scoring

When retrieving transcript snippets for prompt injection, compute a composite relevance score:

```rust
pub struct SnippetRelevanceScore {
    pub total: f32,
    pub components: SnippetScoreComponents,
}

pub struct SnippetScoreComponents {
    pub keyword_score: f32,
    pub recency_bonus: f32,
    pub workspace_bonus: f32,
    pub file_overlap_bonus: f32,
    pub task_kind_bonus: f32,
    pub fts_rank: f32,
    pub diversity_penalty: f32,
}
```

### Scoring formula

```rust
fn score_snippet(
    snippet: &RelevantSnippet,
    query_context: &QueryContext,
    config: &RetrievalConfig,
) -> SnippetRelevanceScore {
    let keyword_score = snippet.fts_score.clamp(0.0, 10.0);
    let recency_bonus = compute_recency_bonus(snippet.conversation_ended_at, config);
    let workspace_bonus = if snippet.workspace == query_context.current_workspace {
        config.same_workspace_bonus
    } else {
        0.0
    };
    let file_overlap_bonus = compute_file_overlap(
        &snippet.file_paths,
        &query_context.declared_paths,
        config,
    );
    let task_kind_bonus = if snippet.task_type == query_context.task_type {
        config.same_task_kind_bonus
    } else {
        0.0
    };
    let diversity_penalty = 0.0; // applied post-scoring in dedup phase

    let total = keyword_score * config.keyword_weight
        + recency_bonus * config.recency_weight
        + workspace_bonus
        + file_overlap_bonus
        + task_kind_bonus;

    SnippetRelevanceScore {
        total,
        components: SnippetScoreComponents {
            keyword_score,
            recency_bonus,
            workspace_bonus,
            file_overlap_bonus,
            task_kind_bonus,
            fts_rank: snippet.fts_score,
            diversity_penalty,
        },
    }
}
```

### Recency bonus

```rust
fn compute_recency_bonus(ended_at: Option<DateTime<Utc>>, config: &RetrievalConfig) -> f32 {
    let Some(ended) = ended_at else { return 0.0 };
    let age_hours = (Utc::now() - ended).num_hours() as f32;
    let half_life_hours = config.recency_half_life_hours as f32;
    if half_life_hours <= 0.0 { return 0.0; }
    let decay = 0.5_f32.powf(age_hours / half_life_hours);
    config.max_recency_bonus * decay
}
```

### File path overlap

```rust
fn compute_file_overlap(
    snippet_paths: &[String],
    declared_paths: &[String],
    config: &RetrievalConfig,
) -> f32 {
    if declared_paths.is_empty() || snippet_paths.is_empty() {
        return 0.0;
    }
    let mut overlap_count = 0;
    for sp in snippet_paths {
        for dp in declared_paths {
            if paths_overlap(sp, dp) {
                overlap_count += 1;
            }
        }
    }
    let overlap_ratio = overlap_count as f32 / declared_paths.len().max(1) as f32;
    (overlap_ratio * config.max_file_overlap_bonus).min(config.max_file_overlap_bonus)
}

fn paths_overlap(a: &str, b: &str) -> bool {
    a == b
        || a.starts_with(b)
        || b.starts_with(a)
        || common_directory_depth(a, b) >= 2
}

fn common_directory_depth(a: &str, b: &str) -> usize {
    let a_parts: Vec<&str> = a.split('/').collect();
    let b_parts: Vec<&str> = b.split('/').collect();
    a_parts.iter().zip(b_parts.iter())
        .take_while(|(x, y)| x == y)
        .count()
}
```

### Retrieval config defaults

```rust
pub struct RetrievalConfig {
    pub keyword_weight: f32,
    pub recency_weight: f32,
    pub max_recency_bonus: f32,
    pub recency_half_life_hours: u32,
    pub same_workspace_bonus: f32,
    pub max_file_overlap_bonus: f32,
    pub same_task_kind_bonus: f32,
    pub max_snippets: usize,
    pub max_snippet_chars: usize,
    pub diversity_threshold: f32,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            keyword_weight: 0.4,
            recency_weight: 0.3,
            max_recency_bonus: 3.0,
            recency_half_life_hours: 168, // 1 week
            same_workspace_bonus: 1.5,
            max_file_overlap_bonus: 2.0,
            same_task_kind_bonus: 0.5,
            max_snippets: 6,
            max_snippet_chars: 2000,
            diversity_threshold: 0.7,
        }
    }
}
```

## 33.2 Playbook Bullet Scoring for Prompt Injection

When selecting playbook bullets for injection, compute a task-specific relevance score on top of the bullet's effective score.

```rust
pub struct BulletInjectionScore {
    pub total: f32,
    pub effective_score: f32,
    pub scope_relevance: f32,
    pub tag_overlap: f32,
    pub recency_bonus: f32,
    pub maturity_weight: f32,
}

fn score_bullet_for_injection(
    bullet: &PlaybookBullet,
    task_context: &TaskContext,
    config: &PlaybookInjectionConfig,
) -> BulletInjectionScore {
    let effective = bullet.effective_score.unwrap_or(0.0);

    let scope_relevance = match bullet.scope {
        BulletScope::Bead => {
            if bullet.source_bead_ids.contains(&task_context.bead_id) { 3.0 }
            else { 0.0 }
        }
        BulletScope::Workspace => {
            if bullet.scope_key.as_deref() == Some(&task_context.workspace) { 2.0 }
            else { 0.5 }
        }
        BulletScope::Language => {
            if task_context.languages.iter().any(|l| bullet.scope_key.as_deref() == Some(l)) { 1.5 }
            else { 0.0 }
        }
        BulletScope::Framework => {
            if task_context.frameworks.iter().any(|f| bullet.scope_key.as_deref() == Some(f)) { 1.5 }
            else { 0.0 }
        }
        BulletScope::Global => 1.0,
    };

    let tag_overlap = compute_tag_overlap(&bullet.tags, &task_context.keywords, config);

    let recency_bonus = compute_bullet_recency(bullet, config);

    let maturity_weight = match bullet.maturity {
        BulletMaturity::Proven => 1.5,
        BulletMaturity::Established => 1.0,
        BulletMaturity::Candidate => 0.3,
        BulletMaturity::Deprecated => 0.0,
    };

    let total = (effective * maturity_weight)
        + (scope_relevance * config.scope_weight)
        + (tag_overlap * config.tag_weight)
        + (recency_bonus * config.recency_weight);

    BulletInjectionScore {
        total,
        effective_score: effective,
        scope_relevance,
        tag_overlap,
        recency_bonus,
        maturity_weight,
    }
}
```

### Tag overlap scoring

```rust
fn compute_tag_overlap(
    bullet_tags: &[String],
    task_keywords: &[String],
    config: &PlaybookInjectionConfig,
) -> f32 {
    if bullet_tags.is_empty() || task_keywords.is_empty() {
        return 0.0;
    }
    let bullet_set: HashSet<&str> = bullet_tags.iter().map(|s| s.as_str()).collect();
    let task_set: HashSet<&str> = task_keywords.iter().map(|s| s.as_str()).collect();
    let overlap = bullet_set.intersection(&task_set).count() as f32;
    let max_overlap = config.max_tag_overlap_bonus;
    (overlap * 0.5).min(max_overlap)
}
```

### Playbook injection config defaults

```rust
pub struct PlaybookInjectionConfig {
    pub scope_weight: f32,
    pub tag_weight: f32,
    pub recency_weight: f32,
    pub max_bullets: usize,
    pub min_score_threshold: f32,
    pub max_tag_overlap_bonus: f32,
    pub prefer_fewer_stronger: bool,
}

impl Default for PlaybookInjectionConfig {
    fn default() -> Self {
        Self {
            scope_weight: 0.3,
            tag_weight: 0.2,
            recency_weight: 0.1,
            max_bullets: 12,
            min_score_threshold: 0.5,
            max_tag_overlap_bonus: 2.0,
            prefer_fewer_stronger: true,
        }
    }
}
```

## 33.3 Diversity-Aware Deduplication

After scoring, deduplicate results to ensure prompt diversity.

```rust
fn deduplicate_snippets(
    scored: &mut Vec<(RelevantSnippet, SnippetRelevanceScore)>,
    config: &RetrievalConfig,
) {
    scored.sort_by(|a, b| b.1.total.partial_cmp(&a.1.total).unwrap_or(Ordering::Equal));

    let mut kept: Vec<usize> = Vec::new();
    let mut conversation_counts: HashMap<i64, usize> = HashMap::new();

    for (idx, (snippet, score)) in scored.iter_mut().enumerate() {
        let conv_count = conversation_counts.entry(snippet.conversation_id).or_insert(0);
        if *conv_count >= 2 {
            score.components.diversity_penalty = -2.0;
            score.total -= 2.0;
            continue;
        }
        let is_too_similar = kept.iter().any(|&kept_idx| {
            jaccard_similarity(
                &scored[kept_idx].0.snippet,
                &snippet.snippet,
            ) > config.diversity_threshold
        });
        if is_too_similar {
            score.components.diversity_penalty = -3.0;
            score.total -= 3.0;
            continue;
        }
        *conv_count += 1;
        kept.push(idx);
    }

    scored.sort_by(|a, b| b.1.total.partial_cmp(&a.1.total).unwrap_or(Ordering::Equal));
}
```

## 33.4 Keyword Extraction from Task Context

Derive search keywords from task metadata for FTS queries:

```rust
pub struct QueryContext {
    pub bead_id: BeadId,
    pub title: String,
    pub description: Option<String>,
    pub declared_paths: Vec<String>,
    pub labels: Vec<String>,
    pub parent_handoff_summaries: Vec<String>,
    pub current_workspace: String,
    pub task_type: String,
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    pub keywords: Vec<String>,
}

fn extract_keywords(ctx: &QueryContext) -> Vec<String> {
    let mut keywords = Vec::new();

    let title_words = tokenize_and_filter(&ctx.title);
    keywords.extend(title_words);

    if let Some(desc) = &ctx.description {
        let desc_words = tokenize_and_filter(desc);
        keywords.extend(desc_words.into_iter().take(10));
    }

    for path in &ctx.declared_paths {
        let segments: Vec<&str> = path.split('/').collect();
        for seg in segments {
            let clean = seg.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if clean.len() >= 3 && !is_stop_word(clean) {
                keywords.push(clean.to_lowercase());
            }
        }
    }

    keywords.extend(ctx.labels.iter().cloned());
    keywords.sort();
    keywords.dedup();
    keywords
}

const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "can", "had",
    "her", "was", "one", "our", "out", "has", "his", "how", "its", "let",
    "may", "new", "now", "old", "see", "way", "who", "did", "get", "got",
    "him", "hit", "say", "she", "too", "use", "src", "mod", "lib", "pub",
    "fn", "impl", "struct", "enum", "type", "self", "super", "crate",
    "with", "this", "that", "from", "into", "have", "been", "will",
    "each", "make", "like", "just", "over", "such", "take", "than",
    "them", "then", "they", "very", "when", "what", "some", "also",
];

fn is_stop_word(word: &str) -> bool {
    STOP_WORDS.contains(&word.to_lowercase().as_str())
}

fn tokenize_and_filter(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= 3)
        .filter(|w| !is_stop_word(w))
        .map(|w| w.to_lowercase())
        .collect()
}
```

## 33.5 Module Mapping

| Concept | Grove module |
|---------|-------------|
| Snippet relevance scoring | `grove-memory::archive::scoring` |
| Bullet injection scoring | `grove-memory::playbook::selector` |
| Diversity deduplication | `grove-memory::archive::retrieval` |
| Keyword extraction | `grove-session::prompt_builder` |
| Retrieval config | `grove-config::model` |

---

## 34. `grove-br` Integration Contract

This section defines the exact CLI parsing, output schemas, and error handling for grove's integration with `br` (beads_rust).

## 34.1 Command Inventory

Grove uses `br` in two modes:

**Read-only** (during scheduling and status):

| Command | Purpose | Output format |
|---------|---------|---------------|
| `br ready --json` | List unblocked beads | JSON array |
| `br list --status open --json` | All open beads | JSON array |
| `br show <id> --json` | Full bead details | JSON object |
| `br dep list <id> --json` | Dependencies for a bead | JSON object |

**Mirror-only** (after task completion):

| Command | Purpose | Output format |
|---------|---------|---------------|
| `br update <id> --status in_progress` | Mark bead started | Text confirmation |
| `br close <id> --reason "<reason>"` | Close completed bead | Text confirmation |
| `br comments add <id> --text "<text>"` | Add completion comment | Text confirmation |
| `br sync --flush-only` | Export to JSONL | Text confirmation |

## 34.2 `br ready --json` Output Schema

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct BrReadyOutput {
    pub issues: Vec<BrReadyIssue>,
    pub count: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BrReadyIssue {
    pub id: String,
    pub title: String,
    pub priority: i32,
    pub issue_type: String,
    pub status: String,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub blocked_by: Vec<String>,
    pub blocks: Vec<String>,
}
```

### Example output

```json
{
  "issues": [
    {
      "id": "bd-e9b1d4",
      "title": "Implement response analyzer",
      "priority": 1,
      "issue_type": "task",
      "status": "open",
      "assignee": null,
      "labels": ["grove-session", "phase-2"],
      "created_at": "2026-03-10T08:00:00Z",
      "updated_at": "2026-03-14T12:00:00Z",
      "blocked_by": [],
      "blocks": ["bd-7f3a2c", "bd-a1bc22"]
    }
  ],
  "count": 1
}
```

## 34.3 `br show <id> --json` Output Schema

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct BrShowOutput {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: i32,
    pub issue_type: String,
    pub status: String,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub close_reason: Option<String>,
    pub blocked_by: Vec<String>,
    pub blocks: Vec<String>,
    pub comments: Vec<BrComment>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BrComment {
    pub id: String,
    pub text: String,
    pub author: Option<String>,
    pub created_at: String,
}
```

## 34.4 BrClient Trait

```rust
#[async_trait::async_trait]
pub trait BrClient: Send + Sync {
    async fn ready(&self) -> anyhow::Result<Vec<BrReadyIssue>>;
    async fn list_open(&self) -> anyhow::Result<Vec<BrReadyIssue>>;
    async fn show(&self, id: &BeadId) -> anyhow::Result<BrShowOutput>;
    async fn dep_list(&self, id: &BeadId) -> anyhow::Result<BrDepOutput>;
    async fn update_status(&self, id: &BeadId, status: &str) -> anyhow::Result<()>;
    async fn close(&self, id: &BeadId, reason: &str) -> anyhow::Result<()>;
    async fn add_comment(&self, id: &BeadId, text: &str) -> anyhow::Result<()>;
    async fn sync_flush(&self) -> anyhow::Result<()>;
    async fn check_available(&self) -> anyhow::Result<BrCapability>;
}

#[derive(Debug, Clone)]
pub struct BrCapability {
    pub available: bool,
    pub version: Option<String>,
    pub beads_dir_exists: bool,
    pub issue_count: Option<usize>,
}
```

## 34.5 CLI Process Spawning

```rust
pub struct CliBrClient {
    br_bin: String,
    working_dir: Utf8PathBuf,
    timeout: Duration,
}

impl CliBrClient {
    async fn run_json<T: DeserializeOwned>(&self, args: &[&str]) -> anyhow::Result<T> {
        let output = tokio::process::Command::new(&self.br_bin)
            .args(args)
            .current_dir(&self.working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("br {} failed: {}", args.join(" "), stderr));
        }

        let stdout = String::from_utf8(output.stdout)?;
        serde_json::from_str(&stdout)
            .map_err(|e| anyhow::anyhow!("Failed to parse br output: {}", e))
    }

    async fn run_text(&self, args: &[&str]) -> anyhow::Result<String> {
        let output = tokio::process::Command::new(&self.br_bin)
            .args(args)
            .current_dir(&self.working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("br {} failed: {}", args.join(" "), stderr));
        }

        Ok(String::from_utf8(output.stdout)?)
    }
}
```

## 34.6 Error Handling for `br` Integration

```rust
#[derive(Debug, thiserror::Error)]
pub enum BrError {
    #[error("br binary not found at {path}")]
    NotFound { path: String },

    #[error("br command failed with exit code {code}: {stderr}")]
    CommandFailed { code: i32, stderr: String },

    #[error("br output parse error: {source}")]
    ParseError { source: serde_json::Error },

    #[error("br timeout after {timeout_secs}s")]
    Timeout { timeout_secs: u64 },

    #[error("no .beads directory found in workspace")]
    NoBeadsDir,

    #[error("bead {id} not found")]
    BeadNotFound { id: String },

    #[error("br version {version} is below minimum {minimum}")]
    VersionTooOld { version: String, minimum: String },
}
```

### Retry rules for `br` commands

| Error type | Retry? | Action |
|------------|--------|--------|
| `NotFound` | no | abort with clear error |
| `CommandFailed` (exit 1) | yes, once | may be transient lock |
| `ParseError` | no | abort with diagnostic |
| `Timeout` | yes, twice | increase timeout |
| `NoBeadsDir` | no | abort with guidance |
| `BeadNotFound` | no | skip this bead |

## 34.7 Bead Cache Sync Protocol

```rust
pub struct SyncResult {
    pub beads_synced: usize,
    pub beads_added: usize,
    pub beads_updated: usize,
    pub beads_removed: usize,
    pub dependencies_updated: usize,
    pub duration_ms: u64,
    pub errors: Vec<SyncError>,
}

pub struct SyncError {
    pub bead_id: Option<String>,
    pub operation: String,
    pub error: String,
}
```

### Sync algorithm

```rust
async fn sync_bead_cache(
    br: &dyn BrClient,
    db: &Database,
) -> anyhow::Result<SyncResult> {
    let mut result = SyncResult::default();
    let start = Instant::now();

    let open_beads = br.list_open().await?;
    let ready_beads = br.ready().await?;
    let ready_set: HashSet<String> = ready_beads.iter().map(|b| b.id.clone()).collect();

    let existing_cache = db.list_cached_beads()?;
    let existing_ids: HashSet<String> = existing_cache.iter().map(|b| b.bead_id.clone()).collect();
    let remote_ids: HashSet<String> = open_beads.iter().map(|b| b.id.clone()).collect();

    for bead in &open_beads {
        let bead_ref = convert_to_bead_ref(bead);
        if existing_ids.contains(&bead.id) {
            db.update_bead_cache(&bead_ref)?;
            result.beads_updated += 1;
        } else {
            db.insert_bead_cache(&bead_ref)?;
            result.beads_added += 1;
        }

        let deps = br.dep_list(&BeadId(bead.id.clone())).await;
        if let Ok(dep_output) = deps {
            db.replace_dependency_snapshot(
                &BeadId(bead.id.clone()),
                &dep_output.blocked_by.iter().map(|s| BeadId(s.clone())).collect::<Vec<_>>(),
                &dep_output.blocks.iter().map(|s| BeadId(s.clone())).collect::<Vec<_>>(),
            )?;
            result.dependencies_updated += 1;
        }

        let is_ready = ready_set.contains(&bead.id);
        let current_grove_status = db.get_grove_status(&BeadId(bead.id.clone()))?;
        if is_ready && current_grove_status == Some(GroveBeadStatus::Idle) {
            db.set_grove_status(&BeadId(bead.id.clone()), GroveBeadStatus::Ready)?;
        }

        result.beads_synced += 1;
    }

    for existing_id in &existing_ids {
        if !remote_ids.contains(existing_id) {
            let grove_status = db.get_grove_status(&BeadId(existing_id.clone()))?;
            if grove_status != Some(GroveBeadStatus::Running)
                && grove_status != Some(GroveBeadStatus::Checkpointed)
            {
                result.beads_removed += 1;
            }
        }
    }

    result.duration_ms = start.elapsed().as_millis() as u64;
    Ok(result)
}
```

## 34.8 Mirror Protocol

After successful task completion, grove mirrors results back to `br`:

```rust
pub struct MirrorAction {
    pub bead_id: BeadId,
    pub action_type: MirrorActionType,
    pub attempted_at: DateTime<Utc>,
    pub succeeded: bool,
    pub error: Option<String>,
}

pub enum MirrorActionType {
    Close { reason: String },
    AddComment { text: String },
    UpdateStatus { status: String },
    SyncFlush,
}

async fn mirror_success(
    br: &dyn BrClient,
    bead_id: &BeadId,
    handoff: &HandoffRecord,
) -> Vec<MirrorAction> {
    let mut actions = Vec::new();

    let comment_text = format!(
        "Grove completed: {}\nArtifacts: {}\nDecisions: {}",
        handoff.summary,
        handoff.artifacts.join(", "),
        handoff.decisions.join(", "),
    );
    let comment_result = br.add_comment(bead_id, &comment_text).await;
    actions.push(MirrorAction {
        bead_id: bead_id.clone(),
        action_type: MirrorActionType::AddComment { text: comment_text },
        attempted_at: Utc::now(),
        succeeded: comment_result.is_ok(),
        error: comment_result.err().map(|e| e.to_string()),
    });

    let close_result = br.close(bead_id, &handoff.summary).await;
    actions.push(MirrorAction {
        bead_id: bead_id.clone(),
        action_type: MirrorActionType::Close { reason: handoff.summary.clone() },
        attempted_at: Utc::now(),
        succeeded: close_result.is_ok(),
        error: close_result.err().map(|e| e.to_string()),
    });

    let flush_result = br.sync_flush().await;
    actions.push(MirrorAction {
        bead_id: bead_id.clone(),
        action_type: MirrorActionType::SyncFlush,
        attempted_at: Utc::now(),
        succeeded: flush_result.is_ok(),
        error: flush_result.err().map(|e| e.to_string()),
    });

    actions
}
```

### Mirror failure handling

If `br close` fails:

1. Record `BrMirrorFailed` event
2. Keep grove's internal `Succeeded` status intact
3. Mark the mirror as pending retry
4. Next coordinator tick will retry the mirror
5. After 3 failed mirror attempts, log a warning and move on

The local handoff is always authoritative. `br` mirror is best-effort.

## 34.9 `br` SQL Schema for Mirror Tracking

```sql
CREATE TABLE br_mirror_actions (
    id TEXT PRIMARY KEY,
    bead_id TEXT NOT NULL,
    action_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    attempted_at TEXT NOT NULL,
    succeeded INTEGER NOT NULL,
    error_message TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    next_retry_at TEXT,
    FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE
);

CREATE INDEX idx_br_mirror_pending ON br_mirror_actions(succeeded, next_retry_at);
CREATE INDEX idx_br_mirror_bead ON br_mirror_actions(bead_id);
```

---

## 35. `grove-bv` Integration Contract

This section defines the exact CLI parsing, output schemas, and scoring integration for grove's use of `bv` (graph-aware triage engine).

## 35.1 Command Inventory

Grove uses `bv` exclusively in robot mode (never interactive TUI):

| Command | Purpose | Frequency |
|---------|---------|-----------|
| `bv --robot-triage` | Full triage with recommendations | Once per coordinator cycle |
| `bv --robot-next` | Single top pick | Quick decision path |
| `bv --robot-plan` | Parallel execution tracks | Parallel scheduling |
| `bv --robot-insights` | Full graph metrics | On-demand for inspect |
| `bv --robot-priority` | Priority misalignment | Periodic audit |
| `bv --robot-alerts` | Stale/blocking issues | Periodic health check |

## 35.2 `bv --robot-triage` Output Schema

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct BvTriageOutput {
    pub quick_ref: BvQuickRef,
    pub recommendations: Vec<BvRecommendation>,
    pub quick_wins: Vec<BvQuickWin>,
    pub blockers_to_clear: Vec<BvBlocker>,
    pub project_health: BvProjectHealth,
    pub commands: Vec<BvCommand>,
    pub data_hash: String,
    pub status: HashMap<String, BvMetricStatus>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BvQuickRef {
    pub total_open: usize,
    pub ready_count: usize,
    pub blocked_count: usize,
    pub in_progress_count: usize,
    pub top_picks: Vec<BvTopPick>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BvTopPick {
    pub id: String,
    pub title: String,
    pub score: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BvRecommendation {
    pub id: String,
    pub title: String,
    pub priority: i32,
    pub score: f64,
    pub reasons: Vec<String>,
    pub unblocks: Vec<String>,
    pub labels: Vec<String>,
    pub page_rank: Option<f64>,
    pub betweenness: Option<f64>,
    pub critical_path_depth: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BvQuickWin {
    pub id: String,
    pub title: String,
    pub effort: String,
    pub impact: String,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BvBlocker {
    pub id: String,
    pub title: String,
    pub blocks_count: usize,
    pub downstream_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BvProjectHealth {
    pub status_distribution: HashMap<String, usize>,
    pub type_distribution: HashMap<String, usize>,
    pub priority_distribution: HashMap<String, usize>,
    pub graph_density: Option<f64>,
    pub longest_chain: Option<usize>,
    pub cycle_count: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BvMetricStatus {
    pub state: String,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BvCommand {
    pub label: String,
    pub command: String,
}
```

## 35.3 `bv --robot-insights` Output Schema

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct BvInsightsOutput {
    pub page_rank: HashMap<String, f64>,
    pub betweenness: HashMap<String, f64>,
    pub hits_authority: HashMap<String, f64>,
    pub hits_hub: HashMap<String, f64>,
    pub eigenvector: HashMap<String, f64>,
    pub critical_path: Vec<BvCriticalPathNode>,
    pub cycles: Vec<Vec<String>>,
    pub k_core: HashMap<String, usize>,
    pub articulation_points: Vec<String>,
    pub slack: HashMap<String, f64>,
    pub status: HashMap<String, BvMetricStatus>,
    pub data_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BvCriticalPathNode {
    pub id: String,
    pub depth: usize,
    pub dependencies_remaining: usize,
}
```

## 35.4 `bv --robot-plan` Output Schema

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct BvPlanOutput {
    pub plan: BvPlan,
    pub data_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BvPlan {
    pub tracks: Vec<BvTrack>,
    pub summary: BvPlanSummary,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BvTrack {
    pub track_id: String,
    pub label: Option<String>,
    pub steps: Vec<BvTrackStep>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BvTrackStep {
    pub bead_id: String,
    pub title: String,
    pub unblocks: Vec<String>,
    pub parallel_with: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BvPlanSummary {
    pub total_tracks: usize,
    pub total_steps: usize,
    pub highest_impact: Option<String>,
    pub estimated_parallel_depth: usize,
}
```

## 35.5 BvClient Trait

```rust
#[async_trait::async_trait]
pub trait BvClient: Send + Sync {
    async fn triage(&self) -> anyhow::Result<BvTriageOutput>;
    async fn next(&self) -> anyhow::Result<BvTopPick>;
    async fn plan(&self) -> anyhow::Result<BvPlanOutput>;
    async fn insights(&self) -> anyhow::Result<BvInsightsOutput>;
    async fn priority_audit(&self) -> anyhow::Result<serde_json::Value>;
    async fn alerts(&self) -> anyhow::Result<serde_json::Value>;
    async fn check_available(&self) -> anyhow::Result<BvCapability>;
}

#[derive(Debug, Clone)]
pub struct BvCapability {
    pub available: bool,
    pub version: Option<String>,
    pub beads_dir_exists: bool,
}
```

## 35.6 Scoring Integration

Grove uses `bv` outputs to enrich its own scheduler scoring:

```rust
fn apply_bv_bonuses(
    candidate: &mut ReadyCandidate,
    triage: &BvTriageOutput,
    insights: Option<&BvInsightsOutput>,
) {
    for rec in &triage.recommendations {
        if rec.id == candidate.bead.bead.id.0 {
            if rec.critical_path_depth.unwrap_or(0) > 3 {
                candidate.bv_bonus += 15;
            }
            if rec.page_rank.unwrap_or(0.0) > 0.1 {
                candidate.bv_bonus += 10;
            }
            if rec.unblocks.len() > 2 {
                candidate.bv_bonus += (rec.unblocks.len() as i32) * 3;
            }
        }
    }

    for blocker in &triage.blockers_to_clear {
        if blocker.id == candidate.bead.bead.id.0 {
            candidate.bv_bonus += (blocker.blocks_count as i32) * 5;
        }
    }

    for qw in &triage.quick_wins {
        if qw.id == candidate.bead.bead.id.0 {
            candidate.bv_bonus += 8;
        }
    }

    if let Some(ins) = insights {
        if let Some(&betweenness) = ins.betweenness.get(&candidate.bead.bead.id.0) {
            if betweenness > 0.05 {
                candidate.bv_bonus += 7;
            }
        }
        if ins.articulation_points.contains(&candidate.bead.bead.id.0) {
            candidate.bv_bonus += 12;
        }
    }
}
```

### Important rule

`bv` bonuses are additive adjustments to grove's scheduler score. They never override `br` readiness. A bead that `br ready` does not list will never be dispatched regardless of `bv` score.

## 35.7 Caching Strategy

`bv` outputs can be expensive to compute. Grove caches them:

```sql
CREATE TABLE bv_cache (
    cache_key TEXT PRIMARY KEY,
    data_hash TEXT NOT NULL,
    output_json TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE INDEX idx_bv_cache_expires ON bv_cache(expires_at);
```

### Cache TTL

| Command | Default TTL |
|---------|-------------|
| `--robot-triage` | 5 minutes |
| `--robot-plan` | 10 minutes |
| `--robot-insights` | 15 minutes |
| `--robot-next` | 2 minutes |
| `--robot-alerts` | 30 minutes |

### Cache invalidation

- On any `br` sync that changes bead count or status
- On `data_hash` mismatch (bv detects underlying data change)
- On explicit `grove run --refresh`

---

## 36. Error Handling and Error Taxonomy

This section defines grove's error handling strategy across all crates.

## 36.1 Error Categories

```rust
#[derive(Debug, thiserror::Error)]
pub enum GroveError {
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),

    #[error("database error: {0}")]
    Database(#[from] DatabaseError),

    #[error("session error: {0}")]
    Session(#[from] SessionError),

    #[error("orchestration error: {0}")]
    Orchestration(#[from] OrchestrationError),

    #[error("memory error: {0}")]
    Memory(#[from] MemoryError),

    #[error("br integration error: {0}")]
    Br(#[from] BrError),

    #[error("bv integration error: {0}")]
    Bv(#[from] BvError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

## 36.2 Config Errors

```rust
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found: {path}")]
    FileNotFound { path: String },

    #[error("TOML parse error: {source}")]
    TomlParse { source: toml::de::Error },

    #[error("validation error: {field} — {message}")]
    Validation { field: String, message: String },

    #[error("workspace root does not exist: {path}")]
    WorkspaceNotFound { path: String },

    #[error("conflicting config: {a} and {b} are incompatible")]
    Conflict { a: String, b: String },
}
```

## 36.3 Database Errors

```rust
#[derive(Debug, thiserror::Error)]
pub enum DatabaseError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("migration failed at version {version}: {reason}")]
    MigrationFailed { version: i32, reason: String },

    #[error("integrity check failed: {findings:?}")]
    IntegrityCheckFailed { findings: Vec<String> },

    #[error("transaction conflict: {0}")]
    TransactionConflict(String),

    #[error("database locked after {timeout_ms}ms")]
    Locked { timeout_ms: u64 },

    #[error("schema version mismatch: expected {expected}, found {found}")]
    SchemaMismatch { expected: i32, found: i32 },
}
```

## 36.4 Session Errors

```rust
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("Claude binary not found: {path}")]
    ClaudeNotFound { path: String },

    #[error("Claude process failed to start: {reason}")]
    SpawnFailed { reason: String },

    #[error("session timed out after {timeout_secs}s")]
    Timeout { timeout_secs: u64 },

    #[error("protocol parse error on line {line}: {reason}")]
    ProtocolParse { line: usize, reason: String },

    #[error("transcript write failed: {0}")]
    TranscriptWrite(std::io::Error),

    #[error("checkpoint payload invalid: {reason}")]
    InvalidCheckpoint { reason: String },

    #[error("context pressure exceeded hard limit: {usage_pct:.1}%")]
    ContextOverflow { usage_pct: f32 },

    #[error("rate limited: retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },

    #[error("permission denied: {detail}")]
    PermissionDenied { detail: String },
}
```

## 36.5 Orchestration Errors

```rust
#[derive(Debug, thiserror::Error)]
pub enum OrchestrationError {
    #[error("leader lease conflict: another leader is active (owner: {owner})")]
    LeaseConflict { owner: String },

    #[error("invalid state transition: {entity} from {from} to {to}")]
    InvalidTransition { entity: String, from: String, to: String },

    #[error("reservation conflict: {requested} conflicts with {held} (held by {held_by})")]
    ReservationConflict { requested: String, held: String, held_by: String },

    #[error("retry budget exhausted for bead {bead_id} after {attempts} attempts")]
    RetryExhausted { bead_id: String, attempts: u32 },

    #[error("circuit breaker open for bead {bead_id}: {reason}")]
    CircuitOpen { bead_id: String, reason: String },

    #[error("recovery error: {0}")]
    Recovery(String),

    #[error("dispatch error: no capacity (running {running}/{max})")]
    NoCapacity { running: usize, max: usize },

    #[error("dependency cycle detected: {cycle:?}")]
    CycleDetected { cycle: Vec<String> },
}
```

## 36.6 Memory Errors

```rust
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("archive ingest failed for {source}: {reason}")]
    IngestFailed { source: String, reason: String },

    #[error("FTS index corrupted: {0}")]
    FtsCorrupted(String),

    #[error("playbook validation failed: {0}")]
    PlaybookValidation(String),

    #[error("scoring computation error: {0}")]
    ScoringError(String),

    #[error("deduplication conflict: bullet {a} and {b}")]
    DedupConflict { a: String, b: String },
}
```

## 36.7 Error Recovery Strategy

| Error type | Recovery action |
|------------|-----------------|
| `Config::FileNotFound` | abort with guidance to run `grove init` |
| `Config::Validation` | abort with specific field and constraint violation |
| `Database::Locked` | retry with exponential backoff (3 attempts, 100ms/500ms/2000ms) |
| `Database::MigrationFailed` | abort; manual intervention required |
| `Database::IntegrityCheckFailed` | log warning, attempt repair, abort if unrecoverable |
| `Session::Timeout` | classify as `FailureClass::Timeout`, trigger retry policy |
| `Session::RateLimited` | classify as `FailureClass::RateLimit`, apply rate-limit backoff |
| `Session::PermissionDenied` | classify as `FailureClass::PermissionDenied`, fail fast |
| `Session::ContextOverflow` | emergency checkpoint, spawn fresh session |
| `Orchestration::LeaseConflict` | abort gracefully with diagnostic |
| `Orchestration::InvalidTransition` | log error, skip transition, emit warning event |
| `Orchestration::CircuitOpen` | wait for cooldown, then half-open retry |
| `Orchestration::RetryExhausted` | mark bead Failed, await manual `grove retry` |
| `Br::NotFound` | abort with installation guidance |
| `Br::CommandFailed` | retry once, then log and continue without sync |
| `Bv::NotFound` | degrade gracefully, skip bv bonuses |
| `Memory::FtsCorrupted` | rebuild FTS index from messages table |

## 36.8 Error Event Logging

Every error above `Warning` severity must be logged as a structured event:

```rust
fn log_error_event(
    db: &Database,
    error: &GroveError,
    context: &ErrorContext,
) -> anyhow::Result<()> {
    let event = EventLogRecord {
        id: 0,
        kind: EventKind::ErrorOccurred,
        bead_id: context.bead_id.clone(),
        run_id: context.run_id.clone(),
        session_id: context.session_id.clone(),
        payload: serde_json::json!({
            "error_type": format!("{:?}", error),
            "message": error.to_string(),
            "severity": classify_severity(error),
            "recoverable": is_recoverable(error),
        }),
        created_at: Utc::now(),
    };
    db.append_event(&event)
}
```

---

## 37. Observability, Metrics, and Diagnostics

Grove must be debuggable in production without attaching a debugger.

## 37.1 Structured Logging

All grove components use `tracing` for structured logging:

```rust
use tracing::{info, warn, error, debug, instrument};

#[instrument(skip(db, br), fields(bead_id = %bead_id.0))]
async fn run_bead(
    bead_id: BeadId,
    db: &Database,
    br: &dyn BrClient,
) -> anyhow::Result<BeadTerminalState> {
    info!(bead_id = %bead_id.0, "Starting bead execution");
    // ...
    warn!(
        bead_id = %bead_id.0,
        context_usage = %estimate.usage_pct,
        "Context pressure approaching threshold"
    );
}
```

### Log levels

| Level | Usage |
|-------|-------|
| `error` | Unrecoverable failures, data corruption, safety incidents |
| `warn` | Recoverable failures, degraded operation, context pressure |
| `info` | Session lifecycle events, bead status changes, sync results |
| `debug` | Protocol parsing details, scoring computations, SQL queries |
| `trace` | Raw stdout/stderr lines, individual line parsing |

### Log output format

```json
{"timestamp":"2026-03-15T10:00:00Z","level":"info","target":"grove_orchestrator::coordinator","bead_id":"bd-e9b1d4","message":"Bead dispatched","score":137,"capacity":"1/2"}
```

## 37.2 Runtime Metrics

Grove tracks operational metrics in SQLite for historical analysis:

```sql
CREATE TABLE metrics (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    metric_name TEXT NOT NULL,
    metric_value REAL NOT NULL,
    labels_json TEXT NOT NULL DEFAULT '{}',
    recorded_at TEXT NOT NULL
);

CREATE INDEX idx_metrics_name_time ON metrics(metric_name, recorded_at);
```

### Core metrics

| Metric | Type | Description |
|--------|------|-------------|
| `grove.sessions.total` | counter | Total sessions spawned |
| `grove.sessions.success` | counter | Sessions that passed exit gate |
| `grove.sessions.failed` | counter | Sessions that failed |
| `grove.sessions.timeout` | counter | Sessions that timed out |
| `grove.sessions.rate_limited` | counter | Sessions rate limited |
| `grove.sessions.duration_secs` | histogram | Session wall-clock duration |
| `grove.beads.completed` | counter | Beads that reached Succeeded |
| `grove.beads.failed` | counter | Beads that reached Failed |
| `grove.beads.retried` | counter | Beads that were retried |
| `grove.context.usage_pct` | gauge | Latest context usage percentage |
| `grove.context.checkpoints` | counter | Checkpoints triggered by pressure |
| `grove.breaker.trips` | counter | Circuit breaker open events |
| `grove.breaker.recoveries` | counter | Circuit breaker close events |
| `grove.scheduler.dispatches` | counter | Beads dispatched per tick |
| `grove.scheduler.queue_depth` | gauge | Ready queue depth |
| `grove.scheduler.tick_duration_ms` | histogram | Scheduler tick duration |
| `grove.memory.bullets_active` | gauge | Active playbook bullets |
| `grove.memory.archive_conversations` | gauge | Indexed conversations |
| `grove.memory.fts_queries` | counter | FTS queries executed |
| `grove.safety.incidents` | counter | Safety incidents detected |
| `grove.br.sync_duration_ms` | histogram | `br` sync duration |
| `grove.br.mirror_failures` | counter | Failed mirror operations |
| `grove.bv.cache_hits` | counter | `bv` cache hits |
| `grove.bv.cache_misses` | counter | `bv` cache misses |

### Metric recording

```rust
pub struct MetricsRecorder {
    db: Database,
    buffer: Vec<MetricEntry>,
    flush_threshold: usize,
}

struct MetricEntry {
    name: String,
    value: f64,
    labels: HashMap<String, String>,
    timestamp: DateTime<Utc>,
}

impl MetricsRecorder {
    pub fn record(&mut self, name: &str, value: f64, labels: &[(&str, &str)]) {
        self.buffer.push(MetricEntry {
            name: name.to_string(),
            value,
            labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            timestamp: Utc::now(),
        });
        if self.buffer.len() >= self.flush_threshold {
            self.flush();
        }
    }

    pub fn flush(&mut self) {
        let entries = std::mem::take(&mut self.buffer);
        for entry in entries {
            let _ = self.db.insert_metric(&entry);
        }
    }
}
```

## 37.3 Health Check API

```rust
pub struct HealthReport {
    pub status: HealthStatus,
    pub components: Vec<ComponentHealth>,
    pub uptime_secs: u64,
    pub leader_active: bool,
    pub last_heartbeat_age_secs: Option<u64>,
}

pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

pub struct ComponentHealth {
    pub name: String,
    pub status: HealthStatus,
    pub message: Option<String>,
    pub last_check: DateTime<Utc>,
}
```

### Health check components

| Component | Healthy | Degraded | Unhealthy |
|-----------|---------|----------|-----------|
| Database | connection ok, schema current | busy_timeout hit once | connection failed, schema mismatch |
| `br` | available, response < 5s | response > 5s | not found or persistent failure |
| `bv` | available, response < 10s | response > 10s | not found (non-fatal) |
| `claude` | binary found on PATH | version check failed | not found |
| Leader lease | active and heartbeat fresh | heartbeat stale (> 2x interval) | expired or conflicting |
| Disk space | > 1GB free in .grove/ | < 1GB free | < 100MB free |

## 37.4 `grove status --health` Output

```text
Health: HEALTHY
Uptime: 2h 15m
Leader: pid-1234@host (heartbeat 2s ago)

Components:
  Database:     OK (v6, 2.1MB, WAL mode)
  br:           OK (v0.4.2, 14 open beads)
  bv:           OK (v0.3.1, cache warm)
  claude:       OK (found at /usr/local/bin/claude)
  Leader lease: OK (expires in 8s)
  Disk space:   OK (12.4GB free)
```

## 37.5 Diagnostic Queries

Pre-built diagnostic queries for debugging:

### Find slow sessions

```sql
SELECT cs.id, cs.run_id, cs.status,
       (julianday(cs.ended_at) - julianday(cs.started_at)) * 86400 AS duration_secs,
       cs.estimated_output_tokens
FROM claude_sessions cs
WHERE cs.ended_at IS NOT NULL
ORDER BY duration_secs DESC
LIMIT 20;
```

### Find beads with most retries

```sql
SELECT bc.bead_id, bc.title, COUNT(tr.id) AS attempt_count,
       MAX(tr.failure_class) AS last_failure
FROM bead_cache bc
JOIN task_runs tr ON tr.bead_id = bc.bead_id
WHERE tr.status = 'Failed'
GROUP BY bc.bead_id
ORDER BY attempt_count DESC
LIMIT 10;
```

### Find most-fired safety incidents

```sql
SELECT pattern_id, category, severity, COUNT(*) AS incident_count
FROM safety_incidents
GROUP BY pattern_id
ORDER BY incident_count DESC
LIMIT 10;
```

### Find most-used playbook bullets

```sql
SELECT pb.id, pb.text, pb.maturity, pb.effective_score,
       COUNT(fe.id) AS feedback_count
FROM playbook_bullets pb
LEFT JOIN feedback_events fe ON fe.bullet_id = pb.id
WHERE pb.deprecated = 0
GROUP BY pb.id
ORDER BY feedback_count DESC
LIMIT 15;
```

### Find stuck beads

```sql
SELECT br.bead_id, bc.title, br.grove_status, br.last_failure_class,
       br.retry_after, br.runtime_updated_at
FROM bead_runtime br
JOIN bead_cache bc ON bc.bead_id = br.bead_id
WHERE br.grove_status IN ('Failed', 'WaitingToRetry')
  AND br.runtime_updated_at < datetime('now', '-1 hour')
ORDER BY br.runtime_updated_at ASC;
```

### Find prompt budget breakdown

```sql
SELECT pm.bead_id, pm.session_id, pm.byte_count,
       pm.segment_manifest_json
FROM prompt_materializations pm
ORDER BY pm.created_at DESC
LIMIT 5;
```

---

## 38. Concurrency Model and Async Architecture

This section defines how grove manages concurrent operations, async task spawning, and resource contention.

## 38.1 Async Runtime

Grove uses Tokio as its async runtime with a multi-threaded executor:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;

    runtime.block_on(async {
        grove_cli::run().await
    })
}
```

### Thread allocation

| Component | Thread model |
|-----------|-------------|
| Coordinator loop | Single Tokio task on main runtime |
| Node runners | One Tokio task per running bead |
| Session runner | One Tokio task per Claude process |
| `br` / `bv` CLI calls | Spawned processes, awaited in Tokio |
| SQLite writes | Serialized through single connection (no async SQLite) |
| Transcript writer | Sync file I/O wrapped in `tokio::task::spawn_blocking` |
| FTS indexing | Background `spawn_blocking` task |

## 38.2 Database Concurrency

SQLite is single-writer. Grove manages this with a centralized database handle:

```rust
pub struct DatabaseHandle {
    conn: parking_lot::Mutex<rusqlite::Connection>,
}

impl DatabaseHandle {
    pub fn with_conn<T>(&self, f: impl FnOnce(&rusqlite::Connection) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let conn = self.conn.lock();
        f(&conn)
    }

    pub fn with_tx<T>(&self, f: impl FnOnce(&rusqlite::Transaction<'_>) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }
}
```

### Why not async SQLite

SQLite operations are fast enough that blocking is acceptable with `parking_lot::Mutex`. The overhead of an async SQLite wrapper (like `tokio-rusqlite`) adds complexity without meaningful benefit for grove's workload.

### Contention mitigation

- Batch related writes into single transactions
- Keep transactions short (no network calls inside a transaction)
- Use `busy_timeout = 5000` PRAGMA for resilience
- WAL mode allows concurrent readers during writes

## 38.3 Session Lifecycle Concurrency

When `max_parallel > 1`, multiple node runners execute concurrently:

```rust
pub struct ConcurrencyManager {
    max_parallel: usize,
    running: Arc<AtomicUsize>,
    semaphore: Arc<Semaphore>,
}

impl ConcurrencyManager {
    pub fn new(max_parallel: usize) -> Self {
        Self {
            max_parallel,
            running: Arc::new(AtomicUsize::new(0)),
            semaphore: Arc::new(Semaphore::new(max_parallel)),
        }
    }

    pub async fn acquire(&self) -> anyhow::Result<ConcurrencyPermit> {
        let permit = self.semaphore.acquire().await?;
        self.running.fetch_add(1, Ordering::Relaxed);
        Ok(ConcurrencyPermit {
            _permit: permit,
            running: self.running.clone(),
        })
    }

    pub fn current_load(&self) -> usize {
        self.running.load(Ordering::Relaxed)
    }

    pub fn available_slots(&self) -> usize {
        self.max_parallel.saturating_sub(self.current_load())
    }
}

pub struct ConcurrencyPermit {
    _permit: tokio::sync::SemaphorePermit<'static>,
    running: Arc<AtomicUsize>,
}

impl Drop for ConcurrencyPermit {
    fn drop(&mut self) {
        self.running.fetch_sub(1, Ordering::Relaxed);
    }
}
```

## 38.4 Event Bus Architecture

The coordinator communicates with node runners through a bounded broadcast channel:

```rust
pub struct EventBus {
    sender: broadcast::Sender<CoordinatorEvent>,
}

#[derive(Debug, Clone)]
pub enum CoordinatorEvent {
    BeadDispatched { bead_id: BeadId, run_id: RunId },
    SessionStarted { session_id: SessionId, bead_id: BeadId },
    SessionCompleted { session_id: SessionId, outcome: SessionTerminalClass },
    CheckpointSaved { bead_id: BeadId, checkpoint_id: CheckpointId },
    HandoffWritten { bead_id: BeadId },
    CircuitBreakerTripped { bead_id: BeadId },
    ReservationConflict { bead_id: BeadId, conflict: ReservationConflict },
    SafetyIncident { session_id: SessionId, severity: PatternSeverity },
    ShutdownRequested,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    pub fn publish(&self, event: CoordinatorEvent) {
        let _ = self.sender.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CoordinatorEvent> {
        self.sender.subscribe()
    }
}
```

## 38.5 Graceful Shutdown

```rust
pub struct ShutdownSignal {
    notify: Arc<tokio::sync::Notify>,
    triggered: Arc<AtomicBool>,
}

impl ShutdownSignal {
    pub fn new() -> Self {
        Self {
            notify: Arc::new(tokio::sync::Notify::new()),
            triggered: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn trigger(&self) {
        self.triggered.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_triggered(&self) -> bool {
        self.triggered.load(Ordering::SeqCst)
    }

    pub async fn wait(&self) {
        if self.is_triggered() { return; }
        self.notify.notified().await;
    }
}
```

### Shutdown sequence

1. SIGINT/SIGTERM received
2. Coordinator sets shutdown signal
3. No new beads dispatched
4. Running sessions receive grace period (30s default)
5. If sessions don't complete, kill Claude processes
6. Persist all pending state to DB
7. Release leader lease
8. Flush metrics and logs
9. Exit with appropriate code

## 38.6 Coordinator Tick Architecture

```rust
pub struct CoordinatorTick {
    pub tick_id: TickId,
    pub started_at: DateTime<Utc>,
    pub sync_result: Option<SyncResult>,
    pub ready_candidates: Vec<ReadyCandidate>,
    pub dispatch_plan: DispatchPlan,
    pub completed_runs: Vec<RunId>,
    pub events_emitted: Vec<EventKind>,
    pub duration_ms: u64,
}

async fn coordinator_loop(
    db: Arc<DatabaseHandle>,
    br: Arc<dyn BrClient>,
    bv: Arc<dyn BvClient>,
    config: Arc<GroveConfig>,
    shutdown: ShutdownSignal,
    event_bus: EventBus,
) -> anyhow::Result<()> {
    let mut tick_counter: u64 = 0;
    let concurrency = ConcurrencyManager::new(config.scheduler.max_parallel);
    let poll_interval = Duration::from_millis(config.scheduler.poll_interval_ms);

    loop {
        if shutdown.is_triggered() {
            info!("Shutdown requested, exiting coordinator loop");
            break;
        }

        tick_counter += 1;
        let tick_id = TickId(format!("tick_{:06}", tick_counter));
        let tick_start = Instant::now();

        expire_stale_reservations(&db)?;

        let sync_result = sync_bead_cache(&*br, &db).await;
        if let Err(e) = &sync_result {
            warn!(error = %e, "Bead cache sync failed, using stale data");
        }

        let triage = bv.triage().await.ok();
        let insights = bv.insights().await.ok();

        let mut candidates = compute_ready_candidates(&db, triage.as_ref(), insights.as_ref())?;
        let capacity = DispatchCapacity {
            max_concurrency: config.scheduler.max_parallel,
            currently_running: concurrency.current_load(),
        };
        let plan = plan_dispatch(&mut candidates, &capacity, &config.scheduler)?;

        for bead_id in &plan.to_dispatch {
            let permit = concurrency.acquire().await?;
            let db_clone = db.clone();
            let config_clone = config.clone();
            let event_bus_clone = event_bus.clone();
            let bead_id_clone = bead_id.clone();

            tokio::spawn(async move {
                let _permit = permit;
                let result = run_bead_node(bead_id_clone.clone(), &db_clone, &config_clone).await;
                match &result {
                    Ok(state) => {
                        event_bus_clone.publish(CoordinatorEvent::HandoffWritten {
                            bead_id: bead_id_clone,
                        });
                    }
                    Err(e) => {
                        error!(bead_id = %bead_id_clone.0, error = %e, "Bead execution failed");
                    }
                }
            });
        }

        let tick_duration = tick_start.elapsed();
        debug!(
            tick_id = %tick_id.0,
            dispatched = plan.to_dispatch.len(),
            ready = candidates.len(),
            running = concurrency.current_load(),
            duration_ms = tick_duration.as_millis(),
            "Coordinator tick completed"
        );

        tokio::select! {
            _ = tokio::time::sleep(poll_interval) => {}
            _ = shutdown.wait() => { break; }
        }
    }

    Ok(())
}
```

---

## 39. Security Model and Permission Boundaries

## 39.1 Principle of Least Privilege

Grove operates with these permission constraints:

| Resource | Grove permission | Rationale |
|----------|-----------------|-----------|
| `.beads/` | Read + mirror writes | `br` owns the data; grove reads and adds comments/closes |
| `.grove/` | Full ownership | Grove's private runtime state |
| Workspace files | Read-only (grove itself) | Claude modifies files, not grove |
| `claude` process | Spawn + terminate | Grove launches and monitors Claude |
| Network | None (MVP) | No HTTP, no remote APIs |
| Environment variables | Selective passthrough | Only configured vars forwarded to Claude |

## 39.2 Environment Variable Passthrough

```rust
fn build_claude_environment(
    config: &RuntimeConfig,
    current_env: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut env = Vec::new();

    let always_pass = [
        "HOME", "USER", "PATH", "SHELL", "TERM",
        "LANG", "LC_ALL", "TZ",
        "ANTHROPIC_API_KEY", "CLAUDE_API_KEY",
    ];
    for key in always_pass {
        if let Some(val) = current_env.get(key) {
            env.push((key.to_string(), val.clone()));
        }
    }

    for key in &config.env_passthrough {
        if let Some(val) = current_env.get(key) {
            env.push((key.to_string(), val.clone()));
        }
    }

    let never_pass = [
        "AWS_SECRET_ACCESS_KEY",
        "GCP_SERVICE_ACCOUNT_KEY",
        "GITHUB_TOKEN",
        "NPM_TOKEN",
        "DOCKER_PASSWORD",
    ];
    env.retain(|(k, _)| !never_pass.contains(&k.as_str()));

    env
}
```

### Rule

Credentials that Claude needs for its work (like `ANTHROPIC_API_KEY`) are passed through. Credentials for infrastructure operations (like `AWS_SECRET_ACCESS_KEY`) are blocked by default unless explicitly allowed via `env_passthrough`.

## 39.3 Workspace Isolation

Each grove session operates within the workspace root:

- Claude's `--working-dir` is set to the workspace root
- Transcript, checkpoint, and artifact paths are always under `.grove/`
- Grove never `cd` to directories outside the workspace
- File paths in reservations are relative to workspace root

## 39.4 Prompt Content Security

Prompts assembled by grove must not leak sensitive data:

```rust
fn sanitize_prompt_content(content: &str) -> String {
    let patterns = [
        (r"(?i)(api[_-]?key|token|secret|password)\s*[=:]\s*\S+", "[REDACTED]"),
        (r"(?i)bearer\s+[a-zA-Z0-9._-]+", "Bearer [REDACTED]"),
        (r"(?i)(sk-|pk-|rk-)[a-zA-Z0-9]{20,}", "[REDACTED_KEY]"),
        (r"-----BEGIN\s+(RSA\s+)?PRIVATE\s+KEY-----[\s\S]*?-----END", "[REDACTED_KEY_BLOCK]"),
    ];

    let mut sanitized = content.to_string();
    for (pattern, replacement) in patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            sanitized = re.replace_all(&sanitized, replacement).to_string();
        }
    }
    sanitized
}
```

### When to sanitize

- Archive snippet retrieval: sanitize before injection
- Playbook bullet text: not sanitized (curated content)
- Checkpoint payloads: sanitize before prompt injection
- Handoff summaries: not sanitized (structured protocol output)

## 39.5 Leader Lock Security

The leader lock prevents multiple orchestrators from racing:

```rust
pub struct LeaderLock {
    lock_file: std::fs::File,
    db_lease: CoordinatorLease,
}

impl LeaderLock {
    pub fn try_acquire(
        lock_path: &Utf8Path,
        db: &DatabaseHandle,
        config: &GroveConfig,
    ) -> anyhow::Result<Option<Self>> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(lock_path)?;

        let locked = file.try_lock_exclusive()
            .map_err(|_| anyhow::anyhow!("lock file contention"))?;

        if !locked {
            return Ok(None);
        }

        let lease = CoordinatorLease {
            workspace_key: config.runtime.workspace_root.clone(),
            owner_id: format!("pid-{}", std::process::id()),
            owner_label: format!("pid-{}@{}", std::process::id(), hostname()),
            acquired_at: Utc::now(),
            heartbeat_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::milliseconds(
                config.scheduler.poll_interval_ms as i64 * 3
            ),
        };

        db.upsert_lease(&lease)?;

        Ok(Some(Self { lock_file: file, db_lease: lease }))
    }

    pub fn heartbeat(&mut self, db: &DatabaseHandle) -> anyhow::Result<()> {
        self.db_lease.heartbeat_at = Utc::now();
        self.db_lease.expires_at = Utc::now() + chrono::Duration::seconds(30);
        db.update_lease_heartbeat(&self.db_lease)
    }

    pub fn release(self, db: &DatabaseHandle) -> anyhow::Result<()> {
        db.delete_lease(&self.db_lease.workspace_key)?;
        drop(self.lock_file);
        Ok(())
    }
}
```

---

## 40. Expanded Testing Strategy

## 40.1 Unit Test Exhaustive Catalog

### `grove-types` tests

```rust
#[cfg(test)]
mod tests {
    #[test] fn bead_id_serde_roundtrip() { ... }
    #[test] fn run_id_serde_roundtrip() { ... }
    #[test] fn session_id_serde_roundtrip() { ... }
    #[test] fn checkpoint_id_serde_roundtrip() { ... }
    #[test] fn priority_ordering() { ... }
    #[test] fn priority_base_score_values() { ... }
    #[test] fn grove_bead_status_all_variants_serialize() { ... }
    #[test] fn run_status_all_variants_serialize() { ... }
    #[test] fn session_status_all_variants_serialize() { ... }
    #[test] fn circuit_state_all_variants_serialize() { ... }
    #[test] fn failure_class_all_variants_serialize() { ... }
    #[test] fn checkpoint_payload_with_all_fields() { ... }
    #[test] fn checkpoint_payload_minimal() { ... }
    #[test] fn protocol_event_result_serde() { ... }
    #[test] fn protocol_event_exit_true_serde() { ... }
    #[test] fn protocol_event_exit_false_serde() { ... }
    #[test] fn protocol_event_checkpoint_serde() { ... }
    #[test] fn bullet_scope_all_variants() { ... }
    #[test] fn bullet_maturity_ordering() { ... }
    #[test] fn feedback_kind_serde() { ... }
}
```

### `grove-config` tests

```rust
#[cfg(test)]
mod tests {
    #[test] fn defaults_are_valid() { ... }
    #[test] fn load_minimal_toml() { ... }
    #[test] fn load_full_toml() { ... }
    #[test] fn validation_rotate_gt_warn() { ... }
    #[test] fn validation_hard_stop_gte_rotate() { ... }
    #[test] fn validation_max_parallel_gte_1() { ... }
    #[test] fn validation_retry_max_gte_1() { ... }
    #[test] fn validation_percentages_in_range() { ... }
    #[test] fn validation_completion_threshold_gte_1() { ... }
    #[test] fn validation_cooldown_minutes_gte_1() { ... }
    #[test] fn relative_path_resolution() { ... }
    #[test] fn absolute_path_preserved() { ... }
    #[test] fn missing_file_returns_error() { ... }
    #[test] fn invalid_toml_returns_parse_error() { ... }
    #[test] fn env_override_applies() { ... }
    #[test] fn conflicting_paths_detected() { ... }
}
```

### `grove-session::protocol` tests

```rust
#[cfg(test)]
mod tests {
    #[test] fn parse_grove_result() { ... }
    #[test] fn parse_grove_result_with_whitespace() { ... }
    #[test] fn parse_grove_exit_true() { ... }
    #[test] fn parse_grove_exit_false() { ... }
    #[test] fn parse_grove_exit_case_insensitive() { ... }
    #[test] fn parse_grove_exit_invalid_value() { ... }
    #[test] fn parse_grove_artifacts_comma_separated() { ... }
    #[test] fn parse_grove_artifacts_json_array() { ... }
    #[test] fn parse_grove_artifacts_single_item() { ... }
    #[test] fn parse_grove_artifacts_empty() { ... }
    #[test] fn parse_grove_lessons_comma_separated() { ... }
    #[test] fn parse_grove_lessons_json_array() { ... }
    #[test] fn parse_grove_decisions_comma_separated() { ... }
    #[test] fn parse_grove_warnings_comma_separated() { ... }
    #[test] fn parse_grove_checkpoint_valid_json() { ... }
    #[test] fn parse_grove_checkpoint_minimal_json() { ... }
    #[test] fn parse_grove_checkpoint_invalid_json() { ... }
    #[test] fn parse_grove_checkpoint_with_context() { ... }
    #[test] fn parse_plain_line_no_marker() { ... }
    #[test] fn parse_line_with_marker_in_middle() { ... }
    #[test] fn parse_repeated_result_overwrites() { ... }
    #[test] fn parse_repeated_artifacts_merges() { ... }
    #[test] fn parse_malformed_marker_logs_warning() { ... }
    #[test] fn parse_marker_with_leading_whitespace() { ... }
    #[test] fn parse_marker_case_sensitive() { ... }
}
```

### `grove-session::circuit_breaker` tests

```rust
#[cfg(test)]
mod tests {
    #[test] fn initial_state_is_closed() { ... }
    #[test] fn no_progress_increments_count() { ... }
    #[test] fn progress_resets_no_progress_count() { ... }
    #[test] fn no_progress_threshold_opens_breaker() { ... }
    #[test] fn same_error_threshold_opens_breaker() { ... }
    #[test] fn permission_denial_threshold_opens_breaker() { ... }
    #[test] fn open_to_half_open_after_cooldown() { ... }
    #[test] fn half_open_to_closed_on_progress() { ... }
    #[test] fn half_open_to_open_on_failure() { ... }
    #[test] fn closed_to_open_on_hard_threshold() { ... }
    #[test] fn progress_in_closed_resets_all_counters() { ... }
    #[test] fn different_error_fingerprint_resets_same_error_count() { ... }
    #[test] fn same_error_fingerprint_increments() { ... }
    #[test] fn cooldown_not_expired_stays_open() { ... }
    #[test] fn multiple_thresholds_earliest_wins() { ... }
}
```

### `grove-session::exit_policy` tests

```rust
#[cfg(test)]
mod tests {
    #[test] fn explicit_exit_false_always_continues() { ... }
    #[test] fn explicit_exit_true_with_indicators_succeeds() { ... }
    #[test] fn explicit_exit_true_without_indicators_continues() { ... }
    #[test] fn no_explicit_exit_with_high_indicators_succeeds_when_not_required() { ... }
    #[test] fn no_explicit_exit_continues_when_required() { ... }
    #[test] fn zero_threshold_any_indicator_succeeds() { ... }
    #[test] fn high_threshold_needs_many_indicators() { ... }
    #[test] fn explicit_exit_false_overrides_everything() { ... }
}
```

### `grove-memory::playbook::scoring` tests

```rust
#[cfg(test)]
mod tests {
    #[test] fn decayed_value_at_zero_age_equals_weight() { ... }
    #[test] fn decayed_value_at_half_life_is_half() { ... }
    #[test] fn decayed_value_at_two_half_lives_is_quarter() { ... }
    #[test] fn effective_score_all_helpful() { ... }
    #[test] fn effective_score_all_harmful() { ... }
    #[test] fn effective_score_mixed_feedback() { ... }
    #[test] fn harmful_multiplier_effect() { ... }
    #[test] fn maturity_scale_candidate_half() { ... }
    #[test] fn maturity_scale_proven_boost() { ... }
    #[test] fn maturity_scale_deprecated_zero() { ... }
    #[test] fn staleness_detection_no_feedback() { ... }
    #[test] fn staleness_detection_old_feedback() { ... }
    #[test] fn staleness_detection_recent_feedback() { ... }
    #[test] fn promotion_candidate_to_established() { ... }
    #[test] fn promotion_established_to_proven() { ... }
    #[test] fn demotion_any_to_deprecated_high_harmful() { ... }
    #[test] fn demotion_negative_score_sustained() { ... }
}
```

### `grove-memory::playbook::curate` tests

```rust
#[cfg(test)]
mod tests {
    #[test] fn exact_dedup_reinforces_existing() { ... }
    #[test] fn approximate_dedup_above_threshold_merges() { ... }
    #[test] fn approximate_dedup_below_threshold_creates_new() { ... }
    #[test] fn conflict_detection_never_vs_always() { ... }
    #[test] fn conflict_detection_avoid_vs_prefer() { ... }
    #[test] fn conflict_detection_exception_markers() { ... }
    #[test] fn delta_add_creates_candidate() { ... }
    #[test] fn delta_helpful_appends_feedback() { ... }
    #[test] fn delta_harmful_appends_feedback_with_reason() { ... }
    #[test] fn delta_replace_deprecates_old() { ... }
    #[test] fn delta_deprecate_sets_state() { ... }
    #[test] fn delta_merge_combines_sources() { ... }
    #[test] fn inversion_harmful_rule_to_antipattern() { ... }
    #[test] fn inversion_threshold_check() { ... }
    #[test] fn jaccard_similarity_identical() { ... }
    #[test] fn jaccard_similarity_no_overlap() { ... }
    #[test] fn jaccard_similarity_partial_overlap() { ... }
    #[test] fn jaccard_similarity_empty_inputs() { ... }
    #[test] fn decision_log_records_all_phases() { ... }
}
```

### `grove-session::safety` tests

```rust
#[cfg(test)]
mod tests {
    #[test] fn detect_rm_rf_root() { ... }
    #[test] fn detect_rm_rf_system_dirs() { ... }
    #[test] fn detect_drop_database() { ... }
    #[test] fn detect_truncate_table() { ... }
    #[test] fn detect_git_force_push() { ... }
    #[test] fn detect_git_force_push_with_lease_is_warning() { ... }
    #[test] fn detect_terraform_destroy() { ... }
    #[test] fn detect_kubectl_delete_node() { ... }
    #[test] fn no_false_positive_on_rm_single_file() { ... }
    #[test] fn no_false_positive_on_git_push() { ... }
    #[test] fn no_false_positive_on_select_from_table() { ... }
    #[test] fn context_capture_before_and_after() { ... }
    #[test] fn severity_summary_counts() { ... }
    #[test] fn apology_keyword_detection() { ... }
    #[test] fn apology_threshold_triggers_flag() { ... }
    #[test] fn redos_protection_rejects_catastrophic() { ... }
    #[test] fn redos_protection_accepts_normal() { ... }
    #[test] fn custom_pattern_validation() { ... }
    #[test] fn disabled_pattern_not_matched() { ... }
}
```

## 40.2 Integration Test Scenarios (Expanded)

### Scenario 1: Single bead full lifecycle

1. init workspace
2. sync one ready bead from mock `br`
3. dispatch to mock Claude backend
4. Claude emits `GROVE_RESULT` + `GROVE_EXIT: true`
5. verify: handoff persisted, bead status Succeeded, mirror attempted

### Scenario 2: Parent-child unblocking

1. sync two beads: parent (ready) and child (blocked)
2. run parent to success
3. re-sync: child now ready
4. run child with parent handoff injected in prompt
5. verify: child receives parent's summary, artifacts, decisions

### Scenario 3: Checkpoint rotation

1. start bead session
2. Claude emits `GROVE_CHECKPOINT` at midpoint
3. verify: checkpoint persisted, session ended
4. new session spawned with checkpoint in prompt
5. second session completes with `GROVE_EXIT: true`
6. verify: final handoff includes work from both sessions

### Scenario 4: Timeout and retry

1. start bead session with 10s timeout
2. mock Claude hangs
3. verify: timeout kills process, session classified as Timeout
4. retry creates new run with new session
5. second attempt succeeds

### Scenario 5: Rate limit retry

1. mock Claude returns rate-limit error
2. verify: classified as RateLimit
3. backoff applies (configurable seconds)
4. next tick retries after backoff

### Scenario 6: Permission denied fail-fast

1. mock Claude returns permission denied
2. verify: classified as PermissionDenied
3. bead immediately marked Failed
4. no automatic retry

### Scenario 7: Circuit breaker activation

1. run 3 sessions with no progress (mock Claude returns same error each time)
2. verify: circuit breaker opens
3. bead not dispatched during cooldown
4. after cooldown: half-open attempt
5. if progress: circuit closes

### Scenario 8: Transcript archive and retrieval

1. complete bead A with detailed Claude output
2. ingest transcript into archive
3. start bead B with overlapping file paths
4. verify: prompt for bead B includes snippet from bead A's transcript

### Scenario 9: Playbook learning across sessions

1. bead A completes with `GROVE_LESSONS: ["Always run tests before committing"]`
2. bead B completes with same lesson
3. bead C completes with same lesson
4. verify: lesson promoted from Candidate to Established
5. bead D receives the lesson in its prompt

### Scenario 10: Process crash and recovery

1. start coordinator with one running bead
2. kill coordinator process
3. restart coordinator
4. verify: recovery scan finds orphaned session
5. if checkpoint exists: bead becomes Checkpointed
6. if no checkpoint: bead becomes Failed or Ready (depending on retry budget)

### Scenario 11: Reservation conflict blocks dispatch

1. bead A running with exclusive reservation on `src/auth/**`
2. bead B becomes ready, also declares `src/auth/**`
3. verify: scheduler does not dispatch bead B while A holds reservation
4. bead A completes, reservation released
5. next tick: bead B dispatched

### Scenario 12: Safety incident detection

1. mock Claude outputs `rm -rf /tmp/test`
2. post-session scan runs
3. verify: safety incident recorded
4. verify: playbook anti-pattern bullet created

### Scenario 13: Parallel execution safety

1. configure `max_parallel = 2`
2. two non-conflicting beads dispatched simultaneously
3. both complete successfully
4. verify: no state corruption, both handoffs persisted

### Scenario 14: Emergency checkpoint on context overflow

1. start session with low `hard_stop_pct` (0.3)
2. mock Claude produces large output exceeding threshold
3. verify: emergency checkpoint synthesized
4. session killed, checkpoint persisted
5. fresh session spawned with emergency checkpoint

### Scenario 15: `br` mirror failure resilience

1. bead completes successfully
2. mock `br close` fails with network error
3. verify: grove keeps Succeeded status locally
4. verify: BrMirrorFailed event logged
5. next tick: mirror retried

## 40.3 Golden Test Fixtures (Expanded)

### Protocol parsing fixtures

```text
tests/golden/protocol/
├── simple_success.input.txt
├── simple_success.expected.json
├── checkpoint_with_context.input.txt
├── checkpoint_with_context.expected.json
├── exit_false_override.input.txt
├── exit_false_override.expected.json
├── multiple_artifacts.input.txt
├── multiple_artifacts.expected.json
├── malformed_checkpoint.input.txt
├── malformed_checkpoint.expected.json
├── mixed_markers_and_text.input.txt
├── mixed_markers_and_text.expected.json
├── json_array_lessons.input.txt
├── json_array_lessons.expected.json
├── empty_output.input.txt
├── empty_output.expected.json
├── only_exit_false.input.txt
├── only_exit_false.expected.json
└── repeated_markers.input.txt
    repeated_markers.expected.json
```

### Safety scanning fixtures

```text
tests/golden/safety/
├── clean_session.input.jsonl
├── clean_session.expected.json
├── rm_rf_detected.input.jsonl
├── rm_rf_detected.expected.json
├── git_force_push.input.jsonl
├── git_force_push.expected.json
├── drop_database.input.jsonl
├── drop_database.expected.json
├── apology_session.input.jsonl
├── apology_session.expected.json
├── mixed_severity.input.jsonl
├── mixed_severity.expected.json
└── false_positive_rm.input.jsonl
    false_positive_rm.expected.json
```

### Scoring fixtures

```text
tests/golden/scoring/
├── bullet_decay_computation.json
├── promotion_candidate_to_established.json
├── demotion_to_deprecated.json
├── snippet_relevance_ranking.json
├── scheduler_score_breakdown.json
└── diversity_dedup_ordering.json
```

## 40.4 Property-Based Tests (Expanded)

```rust
#[cfg(test)]
mod proptests {
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn dependency_dag_acyclic(edges in prop::collection::vec((0..100usize, 0..100usize), 0..50)) {
            // adding edges should detect cycles
        }

        #[test]
        fn reservation_overlap_symmetric(a in ".*", b in ".*") {
            // overlap(a, b) == overlap(b, a)
        }

        #[test]
        fn dedup_hash_idempotent(text in ".*") {
            // normalize(text).hash == normalize(text).hash
        }

        #[test]
        fn recovery_idempotent_on_repeated_scans(state in arb_db_state()) {
            // recover(recover(state)) == recover(state)
        }

        #[test]
        fn scoring_decay_monotonic(age in 0.0..365.0f64, half_life in 1.0..180.0f64) {
            // older events always have lower decayed values
        }

        #[test]
        fn circuit_breaker_reachable_from_any_state(state in arb_circuit_state(), events in arb_events(20)) {
            // no state is permanently unreachable
        }

        #[test]
        fn prompt_budget_never_exceeds_limit(segments in arb_segments(20)) {
            // trimming always brings total under budget
        }

        #[test]
        fn scheduler_deterministic(candidates in arb_candidates(10), config in arb_scheduler_config()) {
            // same inputs always produce same dispatch plan
        }

        #[test]
        fn safety_patterns_no_false_positive_on_common_commands(cmd in arb_safe_command()) {
            // ls, cat, grep, git status, cargo build, npm install should not match
        }
    }
}
```

---

## 41. Configuration Cookbook

Common configuration scenarios with example `grove.toml` snippets.

## 41.1 Single-bead development (simplest)

```toml
[runtime]
claude_bin = "claude"
default_model = "sonnet"
timeout_minutes = 30

[scheduler]
max_parallel = 1
retry_max = 2

[checkpoint]
warn_pct = 0.75
rotate_pct = 0.85

[exit_policy]
require_explicit_exit = true
completion_indicator_threshold = 2
```

## 41.2 Aggressive parallel execution

```toml
[scheduler]
max_parallel = 4
poll_interval_ms = 2000
retry_max = 3
critical_path_bonus = 30

[reservations]
enabled = true
default_ttl_minutes = 45

[checkpoint]
warn_pct = 0.65
rotate_pct = 0.78
```

## 41.3 Conservative production workflow

```toml
[runtime]
timeout_minutes = 120

[scheduler]
max_parallel = 2
retry_max = 5
retry_backoff_secs = 120

[circuit_breaker]
no_progress_threshold = 2
same_error_threshold = 3
cooldown_minutes = 60

[exit_policy]
require_explicit_exit = true
completion_indicator_threshold = 3
heuristic_window = 10

[safety]
scan_transcripts = true
block_on_critical = true

[memory]
enable_playbook = true
max_prompt_bullets = 15
archive_top_k = 8
```

## 41.4 Quick prototyping (relaxed exit)

```toml
[exit_policy]
require_explicit_exit = false
completion_indicator_threshold = 1

[circuit_breaker]
no_progress_threshold = 5
cooldown_minutes = 5

[scheduler]
retry_max = 1
```

## 41.5 Large monorepo with many beads

```toml
[scheduler]
max_parallel = 3
poll_interval_ms = 3000
reservation_conflict_penalty = 2000

[reservations]
enabled = true
default_ttl_minutes = 90

[memory]
archive_top_k = 10
max_prompt_snippets = 5
max_prompt_bullets = 20
```

## 41.6 Opus model for complex tasks

```toml
[runtime]
default_model = "opus"
timeout_minutes = 180

[checkpoint]
warn_pct = 0.60
rotate_pct = 0.75
hard_stop_pct = 0.85
```

---

## 42. Glossary and Term Definitions

| Term | Definition |
|------|-----------|
| **Bead** | An issue in the `br` issue tracker. The atomic unit of work that grove orchestrates. |
| **BeadId** | Unique identifier for a bead, e.g., `bd-e9b1d4`. Owned by `br`. |
| **Run** | A single attempt by grove to complete a bead. One bead may have many runs. |
| **RunId** | Unique identifier for a run, e.g., `run_01HXYZ`. Generated by grove. |
| **Session** | A single Claude CLI invocation. One run may have many sessions (due to checkpoints). |
| **SessionId** | Unique identifier for a session, e.g., `ses_01HXYZ`. Generated by grove. |
| **Checkpoint** | A snapshot of partial progress emitted by Claude, enabling continuation in a fresh session. |
| **Handoff** | The final structured output of a completed bead, consumed by dependent beads. |
| **Reservation** | A file path claim that prevents conflicting parallel execution. |
| **Circuit Breaker** | A safety mechanism that stops execution when progress stalls. |
| **Playbook** | A collection of evidence-scored lessons learned from past sessions. |
| **Bullet** | A single rule or anti-pattern in the playbook. |
| **Maturity** | The evidence level of a bullet: Candidate, Established, Proven, Deprecated. |
| **FTS** | Full-Text Search using SQLite FTS5 for transcript archive retrieval. |
| **Protocol Marker** | A `GROVE_*` prefixed line that Claude emits for structured communication. |
| **Exit Gate** | The logic that determines when a session has truly completed its task. |
| **Dispatch** | The act of assigning a ready bead to a Claude session for execution. |
| **Ready Set** | The set of beads that are unblocked (per `br`) and not locally blocked by grove. |
| **Coordinator** | The main orchestration loop that manages dispatch, recovery, and lifecycle. |
| **Leader Lease** | A lock ensuring only one coordinator instance runs per workspace. |
| **Mirror** | The act of writing grove's results back to `br` (close, comment, etc.). |
| **Diary** | A structured summary of a run's outcome, used for implicit playbook feedback. |
| **Tick** | One iteration of the coordinator's main loop. |
| **Score Breakdown** | An explainable decomposition of a bead's dispatch priority. |
| **Emergency Checkpoint** | A synthetic checkpoint created when a session crashes or exceeds context limits. |
| **Context Pressure** | An estimate of how full the Claude context window is, triggering checkpoints. |
| **Apology Detection** | Scanning transcripts for language indicating Claude made mistakes. |
| **Safety Incident** | A detected destructive command pattern in a session transcript. |
| **Deprecated Pattern** | A coding pattern that should no longer be used, tracked for prompt warnings. |
| **WAL** | Write-Ahead Logging mode for SQLite, enabling concurrent reads during writes. |
| **RRF** | Reciprocal Rank Fusion, a method for combining lexical and semantic search results (post-MVP). |
| **Jaccard Similarity** | Set-based similarity metric used for deduplication (`|A ∩ B| / |A ∪ B|`). |
| **Decay** | Exponential time-based reduction of feedback weight: `weight × 0.5^(age/half_life)`. |
| **`br`** | beads_rust CLI — the authoritative issue tracker. |
| **`bv`** | Graph-aware triage engine for beads — computes PageRank, critical path, etc. |
| **`grove`** | This tool — beads-backed Claude workflow orchestrator. |

---

## 43. Appendix: Full `0001_init.sql` Migration

This consolidates all MVP tables into a single initial migration file for reference.

```sql
-- Grove initial schema
-- Version: 1
-- Applies: all MVP tables

PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA synchronous = NORMAL;
PRAGMA temp_store = MEMORY;
PRAGMA busy_timeout = 5000;

-- Migration tracking
CREATE TABLE IF NOT EXISTS _migrations (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Bead cache (mirrors br)
CREATE TABLE bead_cache (
    bead_id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    description TEXT,
    priority INTEGER NOT NULL,
    issue_type TEXT NOT NULL,
    status TEXT NOT NULL,
    assignee TEXT,
    labels_json TEXT NOT NULL DEFAULT '[]',
    parent_ids_json TEXT NOT NULL DEFAULT '[]',
    dependency_ids_json TEXT NOT NULL DEFAULT '[]',
    dependent_ids_json TEXT NOT NULL DEFAULT '[]',
    raw_json TEXT NOT NULL,
    synced_at TEXT NOT NULL
);

CREATE INDEX idx_bead_cache_status ON bead_cache(status);
CREATE INDEX idx_bead_cache_priority_status ON bead_cache(priority, status);

-- Grove runtime state
CREATE TABLE bead_runtime (
    bead_id TEXT PRIMARY KEY,
    grove_status TEXT NOT NULL,
    declared_paths_json TEXT NOT NULL DEFAULT '[]',
    metadata_json TEXT NOT NULL DEFAULT '{}',
    last_run_id TEXT,
    retry_after TEXT,
    last_failure_class TEXT,
    last_failure_detail TEXT,
    runtime_updated_at TEXT NOT NULL,
    FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE
);

CREATE INDEX idx_bead_runtime_status ON bead_runtime(grove_status);
CREATE INDEX idx_bead_runtime_retry_after ON bead_runtime(retry_after);

-- Dependency snapshot
CREATE TABLE bead_dependencies (
    parent_id TEXT NOT NULL,
    child_id TEXT NOT NULL,
    relation_type TEXT NOT NULL DEFAULT 'blocks',
    synced_at TEXT NOT NULL,
    PRIMARY KEY (parent_id, child_id, relation_type)
);

CREATE INDEX idx_bead_dependencies_child ON bead_dependencies(child_id);
CREATE INDEX idx_bead_dependencies_parent ON bead_dependencies(parent_id);

-- Task runs
CREATE TABLE task_runs (
    id TEXT PRIMARY KEY,
    bead_id TEXT NOT NULL,
    attempt_no INTEGER NOT NULL,
    status TEXT NOT NULL,
    failure_class TEXT,
    failure_detail TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    session_count INTEGER NOT NULL DEFAULT 0,
    checkpoint_count INTEGER NOT NULL DEFAULT 0,
    last_checkpoint_id TEXT,
    FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE
);

CREATE INDEX idx_task_runs_bead_attempt ON task_runs(bead_id, attempt_no);
CREATE INDEX idx_task_runs_status ON task_runs(status);

-- Claude sessions
CREATE TABLE claude_sessions (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    external_session_id TEXT,
    ordinal_in_run INTEGER NOT NULL,
    status TEXT NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    prompt_bytes INTEGER NOT NULL DEFAULT 0,
    estimated_input_tokens INTEGER NOT NULL DEFAULT 0,
    estimated_output_tokens INTEGER NOT NULL DEFAULT 0,
    exit_code INTEGER,
    stop_reason TEXT,
    transcript_path TEXT NOT NULL,
    FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

CREATE INDEX idx_claude_sessions_run_ordinal ON claude_sessions(run_id, ordinal_in_run);
CREATE INDEX idx_claude_sessions_external ON claude_sessions(external_session_id);

-- Checkpoints
CREATE TABLE checkpoints (
    id TEXT PRIMARY KEY,
    bead_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    progress TEXT NOT NULL,
    next_step TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    saved_at TEXT NOT NULL,
    resume_generation INTEGER NOT NULL,
    FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE,
    FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (session_id) REFERENCES claude_sessions(id) ON DELETE CASCADE
);

CREATE INDEX idx_checkpoints_bead_saved ON checkpoints(bead_id, saved_at DESC);
CREATE INDEX idx_checkpoints_run_saved ON checkpoints(run_id, saved_at DESC);

-- Handoffs
CREATE TABLE handoffs (
    bead_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    summary TEXT NOT NULL,
    artifacts_json TEXT NOT NULL,
    lessons_json TEXT NOT NULL,
    decisions_json TEXT NOT NULL,
    warnings_json TEXT NOT NULL,
    completed_at TEXT NOT NULL,
    FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE,
    FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

-- Reservations
CREATE TABLE reservations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    bead_id TEXT NOT NULL,
    run_id TEXT,
    path_pattern TEXT NOT NULL,
    exclusive INTEGER NOT NULL,
    reason TEXT,
    expires_at TEXT NOT NULL,
    released_at TEXT,
    FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE,
    FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE SET NULL
);

CREATE INDEX idx_reservations_active ON reservations(released_at, expires_at);
CREATE INDEX idx_reservations_bead ON reservations(bead_id);

-- Event log
CREATE TABLE event_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    bead_id TEXT,
    run_id TEXT,
    session_id TEXT,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_event_log_bead ON event_log(bead_id, id);
CREATE INDEX idx_event_log_run ON event_log(run_id, id);
CREATE INDEX idx_event_log_session ON event_log(session_id, id);
CREATE INDEX idx_event_log_kind_created ON event_log(kind, created_at);

-- Coordinator leases
CREATE TABLE coordinator_leases (
    workspace_key TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,
    owner_label TEXT NOT NULL,
    acquired_at TEXT NOT NULL,
    heartbeat_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX idx_coordinator_leases_expires ON coordinator_leases(expires_at);

-- Prompt materializations
CREATE TABLE prompt_materializations (
    id TEXT PRIMARY KEY,
    bead_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    prompt_path TEXT NOT NULL,
    prompt_hash TEXT NOT NULL,
    byte_count INTEGER NOT NULL,
    segment_manifest_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE,
    FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (session_id) REFERENCES claude_sessions(id) ON DELETE CASCADE
);

CREATE INDEX idx_prompt_materializations_bead ON prompt_materializations(bead_id, created_at DESC);

-- Dispatch decisions
CREATE TABLE dispatch_decisions (
    id TEXT PRIMARY KEY,
    bead_id TEXT NOT NULL,
    tick_id TEXT NOT NULL,
    disposition TEXT NOT NULL,
    score_breakdown_json TEXT NOT NULL,
    blocking_reasons_json TEXT NOT NULL DEFAULT '[]',
    competing_bead_ids_json TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE
);

CREATE INDEX idx_dispatch_decisions_bead ON dispatch_decisions(bead_id, created_at DESC);
CREATE INDEX idx_dispatch_decisions_tick ON dispatch_decisions(tick_id);

-- Archive watermarks
CREATE TABLE archive_watermarks (
    source_id TEXT PRIMARY KEY,
    source_kind TEXT NOT NULL,
    source_label TEXT NOT NULL,
    last_cursor TEXT,
    last_ingested_at TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}'
);

-- Config snapshots
CREATE TABLE config_snapshots (
    id TEXT PRIMARY KEY,
    sha256 TEXT NOT NULL,
    source_path TEXT NOT NULL,
    config_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE UNIQUE INDEX idx_config_snapshots_sha256 ON config_snapshots(sha256);

-- Integrity checks
CREATE TABLE integrity_checks (
    id TEXT PRIMARY KEY,
    scope TEXT NOT NULL,
    scope_key TEXT,
    status TEXT NOT NULL,
    findings_json TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL
);

CREATE INDEX idx_integrity_checks_scope ON integrity_checks(scope, scope_key, created_at DESC);

-- Archive: sources
CREATE TABLE sources (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    label TEXT NOT NULL,
    origin_host TEXT,
    created_at TEXT NOT NULL
);

-- Archive: conversations
CREATE TABLE conversations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    bead_id TEXT,
    run_id TEXT,
    session_id TEXT NOT NULL UNIQUE,
    source_id TEXT NOT NULL,
    workspace_path TEXT,
    title TEXT,
    source_path TEXT NOT NULL,
    started_at INTEGER,
    ended_at INTEGER,
    approx_tokens INTEGER,
    metadata_json TEXT NOT NULL,
    FOREIGN KEY (source_id) REFERENCES sources(id),
    FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE SET NULL
);

CREATE INDEX idx_conversations_bead ON conversations(bead_id);
CREATE INDEX idx_conversations_run ON conversations(run_id);

-- Archive: messages
CREATE TABLE messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id INTEGER NOT NULL,
    idx INTEGER NOT NULL,
    role TEXT NOT NULL,
    author TEXT,
    created_at INTEGER,
    content TEXT NOT NULL,
    extra_json TEXT NOT NULL,
    FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
);

CREATE INDEX idx_messages_conv_idx ON messages(conversation_id, idx);

-- Archive: snippets
CREATE TABLE snippets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id INTEGER NOT NULL,
    file_path TEXT,
    start_line INTEGER,
    end_line INTEGER,
    language TEXT,
    snippet_text TEXT,
    FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX idx_snippets_message ON snippets(message_id);
CREATE INDEX idx_snippets_file_path ON snippets(file_path);

-- Archive: FTS
CREATE VIRTUAL TABLE fts_messages USING fts5(
    content,
    author,
    tokenize = 'unicode61'
);

-- Playbook: bullets
CREATE TABLE playbook_bullets (
    id TEXT PRIMARY KEY,
    scope TEXT NOT NULL,
    scope_key TEXT,
    category TEXT NOT NULL,
    text TEXT NOT NULL,
    bullet_type TEXT NOT NULL,
    state TEXT NOT NULL,
    maturity TEXT NOT NULL,
    helpful_count INTEGER NOT NULL DEFAULT 0,
    harmful_count INTEGER NOT NULL DEFAULT 0,
    feedback_events_json TEXT NOT NULL DEFAULT '[]',
    confidence_decay_half_life_days INTEGER NOT NULL DEFAULT 90,
    pinned INTEGER NOT NULL DEFAULT 0,
    deprecated INTEGER NOT NULL DEFAULT 0,
    replaced_by TEXT,
    deprecation_reason TEXT,
    source_bead_ids_json TEXT NOT NULL DEFAULT '[]',
    source_run_ids_json TEXT NOT NULL DEFAULT '[]',
    tags_json TEXT NOT NULL DEFAULT '[]',
    effective_score REAL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (replaced_by) REFERENCES playbook_bullets(id)
);

CREATE INDEX idx_playbook_maturity ON playbook_bullets(maturity);
CREATE INDEX idx_playbook_scope ON playbook_bullets(scope, scope_key);
CREATE INDEX idx_playbook_deprecated ON playbook_bullets(deprecated);

-- Playbook: feedback events
CREATE TABLE feedback_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    bullet_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    weight REAL NOT NULL DEFAULT 1.0,
    bead_id TEXT,
    run_id TEXT,
    context TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY (bullet_id) REFERENCES playbook_bullets(id) ON DELETE CASCADE
);

CREATE INDEX idx_feedback_events_bullet ON feedback_events(bullet_id, created_at DESC);

-- Playbook: diaries
CREATE TABLE memory_diaries (
    id TEXT PRIMARY KEY,
    bead_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    outcome TEXT NOT NULL,
    summary TEXT NOT NULL,
    accomplishments_json TEXT NOT NULL,
    decisions_json TEXT NOT NULL,
    challenges_json TEXT NOT NULL,
    key_learnings_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE,
    FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE CASCADE
);

-- Safety: incidents
CREATE TABLE safety_incidents (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    bead_id TEXT NOT NULL,
    pattern_id TEXT NOT NULL,
    category TEXT NOT NULL,
    severity TEXT NOT NULL,
    matched_text TEXT NOT NULL,
    transcript_line INTEGER NOT NULL,
    context_json TEXT NOT NULL,
    disposition TEXT NOT NULL DEFAULT 'detected',
    reviewed_at TEXT,
    reviewed_by TEXT,
    review_note TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_safety_incidents_session ON safety_incidents(session_id);
CREATE INDEX idx_safety_incidents_severity ON safety_incidents(severity, created_at DESC);

-- Safety: deprecated patterns
CREATE TABLE deprecated_patterns (
    id TEXT PRIMARY KEY,
    pattern TEXT NOT NULL,
    pattern_type TEXT NOT NULL,
    scope TEXT NOT NULL,
    scope_key TEXT,
    description TEXT NOT NULL,
    replacement TEXT,
    deprecated_at TEXT NOT NULL,
    reason TEXT NOT NULL,
    source_bullet_id TEXT,
    active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    FOREIGN KEY (source_bullet_id) REFERENCES playbook_bullets(id) ON DELETE SET NULL
);

CREATE INDEX idx_deprecated_patterns_scope ON deprecated_patterns(scope, scope_key);
CREATE INDEX idx_deprecated_patterns_active ON deprecated_patterns(active);

-- Mirror tracking
CREATE TABLE br_mirror_actions (
    id TEXT PRIMARY KEY,
    bead_id TEXT NOT NULL,
    action_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    attempted_at TEXT NOT NULL,
    succeeded INTEGER NOT NULL,
    error_message TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    next_retry_at TEXT,
    FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE
);

CREATE INDEX idx_br_mirror_pending ON br_mirror_actions(succeeded, next_retry_at);

-- BV cache
CREATE TABLE bv_cache (
    cache_key TEXT PRIMARY KEY,
    data_hash TEXT NOT NULL,
    output_json TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE INDEX idx_bv_cache_expires ON bv_cache(expires_at);

-- Metrics
CREATE TABLE metrics (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    metric_name TEXT NOT NULL,
    metric_value REAL NOT NULL,
    labels_json TEXT NOT NULL DEFAULT '{}',
    recorded_at TEXT NOT NULL
);

CREATE INDEX idx_metrics_name_time ON metrics(metric_name, recorded_at);

-- Record this migration
INSERT INTO _migrations (version, name) VALUES (1, 'init');
```

---

## End of Plan

This document is the single authoritative design reference for grove. All patterns, data models, state machines, algorithms, SQL schemas, and module mappings needed to implement grove are contained here. No external reference projects are required.
