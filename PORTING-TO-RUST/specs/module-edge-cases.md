# Edge Cases Catalog — Looper Rust Port

> Ngôn ngữ: Tiếng Việt (với thuật ngữ kỹ thuật tiếng Anh).
> Nguồn: Research từ Go codebase — `internal/storage/`, `internal/agent/executor.go`, `internal/config/`, `internal/network/`, `internal/infra/git/`, `internal/runtime/`, `internal/webhookforward/`
> Format: Mỗi edge case có ID, category, severity, description, impact, detection, Rust mitigation, test strategy.

---

## 1. SQLite Edge Cases

### EC-001: SQLITE_BUSY on concurrent queue operations
- **Category**: SQLite
- **Severity**: HIGH
- **Description**: Scheduler tick's `claimNext()` runs concurrently with webhook forwarder's `createOrGetActiveByDedupe()`. Both write to `queue_items` table. Go dùng `_txlock=immediate` trong DSN (acquires write lock at BEGIN time). `busy_timeout=5000ms` (5s). Nếu lock held >5s, operation fails với `SQLITE_BUSY`. Connection pool: max 4 connections, WAL mode cho phép concurrent readers.
- **Impact**: Queue item không được claim trong tick này. Retry tick sau (30s delay). Transient nhưng add latency.
- **Detection**: `log.Error("sqlite busy on claim")` với retry count
- **Rust Mitigation**: 
  - `rusqlite` với `busy_timeout=5000`
  - WAL journal mode (`PRAGMA journal_mode=WAL`)
  - `_txlock=immediate` trong DSN (giống Go)
  - Retry claim operation 3x với 100ms backoff
  - `deadpool-sqlite` connection pool (pool size 4)
- **Test**: Concurrent claim test: 10 threads claim từ cùng pool, verify exactly-one-claim-per-item

### EC-002: Migration version mismatch on startup
- **Category**: SQLite
- **Severity**: CRITICAL
- **Description**: Go dùng `refinery` migration runner. Migrations từ `0001` đến `0017`. Daemon startup run migrations sequentially. Nếu DB schema version cao hơn binary version (downgrade), hoặc version thấp hơn (upgrade gap), migration runner fail. Go không có downgrade support — migrations forward-only.
- **Impact**: Daemon không start được. User phải manual restore backup hoặc rollback binary.
- **Detection**: `schema_migrations` table checksum mismatch, log error "migration failed"
- **Rust Mitigation**:
  - `refinery` crate (Rust port) cho migration framework
  - Embedded migrations (include_str! hoặc include_dir!)
  - Startup: verify applied_migrations count >= binary_migrations count
  - Nếu mismatch: error message chi tiết (expected N, got M)
  - Không auto-downgrade — fail fast
- **Test**: Test migration từ version 0→N, test DB từ Go-generated schema, test rollback scenario

### EC-003: UUIDv4 / event ID collision
- **Category**: SQLite
- **Severity**: LOW
- **Description**: Event IDs format `event_{16_hex}` từ `crypto/rand`. PRIMARY KEY constraint. Xác suất collision cực thấp nhưng Go có fallback: nếu `rand.Read` fail → `event_{unix_nano}`. Với nano-second resolution, concurrent events cùng nanosecond có thể collision.
- **Impact**: Duplicate PRIMARY KEY → INSERT fails → event mất.
- **Detection**: `UNIQUE constraint failed` on event_logs insert
- **Rust Mitigation**:
  - `uuid` crate (v4) cho event IDs (128-bit, collision probability negligible)
  - PRIMARY KEY = `TEXT` với UUID string
  - Fallback: `SystemTime::now().duration_since(UNIX_EPOCH).as_nanos()` + thread_id suffix
  - Retry insert with new ID on constraint violation
- **Test**: Test random ID generation, test fallback path

### EC-004: Large number of queue items / events (data growth)
- **Category**: SQLite
- **Severity**: MEDIUM
- **Description**: Looper không có data retention policy cho event_logs hoặc old queue items. Over time, `event_logs`, `runs`, `agent_executions` tables grow unbounded. No `VACUUM` or cleanup mechanism in Go.
- **Impact**: DB file grows indefinitely (potentially GBs). Query performance degrades on full table scans (no index on some query patterns).
- **Detection**: DB file size monitoring (external), slow queries
- **Rust Mitigation**:
  - Retention policy config: `maxEventLogDays` (default 90), `maxRunRecordDays` (default 30)
  - Background cleanup task: delete old records + `PRAGMA optimize` periodically
  - Index all query patterns (review existing indexes from module2)
  - Optional: auto-vacuum (`PRAGMA auto_vacuum=INCREMENTAL`)
- **Test**: Insert 10K+ events, verify query performance, test retention cleanup

### EC-005: Unicode repo paths in storage
- **Category**: SQLite
- **Severity**: LOW
- **Description**: `repo_path` field trong `projects` table có thể chứa Unicode (e.g., `café/repo`). SQLite handles Unicode natively (TEXT), nhưng index matching case-sensitivity khác nhau giữa platforms.
- **Impact**: Project lookup by repo_path fail if case mismatch (NFC vs NFD normalization).
- **Detection**: Strange "project not found" errors with Unicode names
- **Rust Mitigation**:
  - Normalize repo_path: NFC normalization trước khi store
  - Case-insensitive COLLATE NOCASE cho repo_path lookups
  - Consistent encoding throughout
- **Test**: Test with Unicode repo names, different normalization forms

### EC-006: Concurrent transaction deadlock (multi-table write)
- **Category**: SQLite
- **Severity**: MEDIUM
- **Description**: Một số operations write vào multiple tables trong cùng transaction: `createRun()` writes loops + runs + event_logs. Nếu two concurrent operations acquire locks in different order → SQLite detects deadlock → one transaction gets SQLITE_BUSY.
- **Impact**: One operation fails, retried later
- **Detection**: SQLITE_BUSY with "deadlock" in message
- **Rust Mitigation**:
  - `_txlock=immediate` (Go pattern) — force write lock at BEGIN
  - Consistent table access order across all transactions
  - Short transaction duration (no .await inside transaction)
- **Test**: Concurrent write test với nhiều tables

---

## 2. System / Platform Edge Cases

### EC-010: Symlink traversal in worktree path
- **Category**: System
- **Severity**: CRITICAL
- **Description**: Worktree paths có thể chứa symlinks. Go dùng `filepath.EvalSymlinks` recursively (depth limit 255). Nếu không resolve, symlink có thể trỏ ra ngoài worktree root → path traversal → write files outside sandbox.
- **Impact**: Security breach — agent writes outside worktree to arbitrary filesystem location
- **Detection**: worktreesafety.Validate() returns error
- **Rust Mitigation**:
  - `std::fs::canonicalize()` — resolves symlinks
  - Custom depth-limited symlink resolution (limit 255)
  - Verify resolved path starts with worktree root
  - All 7 safety checks từ module-worktree-safety.md
- **Test**: Test với symlink chains, circular symlinks, symlink outside root

### EC-011: Disk full during write operations
- **Category**: System
- **Severity**: HIGH
- **Description**: SQLite write fails (ENOSPC), log write fails, file creation fails. Go không có graceful handling — error propagates up. Worktree operations (git checkout, file writes) fail partially.
- **Impact**: Data loss (partial write), daemon crash, corrupt state
- **Detection**: OS error "no space left on device"
- **Rust Mitigation**:
  - Log: try stderr, then silent (no crash)
  - SQLite: mark queue items manual_intervention
  - Daemon: enter "read-only" mode (stop accepting new work, allow read queries)
  - Monitor: warn on low disk space at startup
  - Worktree: git operations fail with clear message
- **Test**: Test with filesystem quota, simulate ENOSPC

### EC-012: Clock skew between timestamps
- **Category**: System
- **Severity**: MEDIUM
- **Description**: Event timestamps dùng local timezone (log) vs UTC (event_log ISO string). Clock jump backward (NTP sync) → timestamps go backward → ordering assumptions violated. Queue items sorted by `available_at` may be stuck waiting for future time.
- **Impact**: Events out of order, queue items stuck (available_at in future)
- **Detection**: Negative duration computations, `next_run_at` in past
- **Rust Mitigation**:
  - ALL timestamps: UTC (`chrono::Utc::now()`)
  - Single time source throughout app (injectable `Clock` trait)
  - Queue claim: `WHERE available_at <= datetime('now')` — immune to clock jump
  - Event ordering: use auto-increment id as tiebreaker for same-timestamp events
- **Test**: Test with mocked clock that jumps backward, verify queue behavior

### EC-013: Zombie processes (agent children)
- **Category**: System
- **Severity**: MEDIUM
- **Description**: Agent process spawns child processes (git, gh). When agent killed via SIGTERM→SIGKILL, grandchildren may become orphaned zombies. Go uses `Setpgid: true` để kill cả process group, nhưng grandchildren that change session (e.g., daemonizing) escape.
- **Impact**: Zombie processes accumulate, FD leak, PID exhaustion
- **Detection**: `ps aux | grep defunct`
- **Rust Mitigation**:
  - `tokio::process::Command` với `kill_on_drop(true)`
  - Process group kill: `libc::kill(-pid, SIGTERM)` (Unix only)
  - Track children PIDs in ActiveExecutionRegistry → kill on shutdown
  - Platform-specific: macOS vs Linux process group handling
- **Test**: Test process tree cleanup after agent kill, verify no zombies

### EC-014: Cross-platform signal differences (SIGTERM, SIGKILL)
- **Category**: System
- **Severity**: HIGH
- **Description**: Go code dùng `syscall.SIGTERM` và `syscall.SIGKILL` cho agent timeout. POSIX signals hoạt động trên macOS/Linux nhưng khác trên Windows. Process group kill (`syscall.Kill(-pid, ...)`) là Unix-specific.
- **Impact**: Rust port không portable nếu không handle OS differences. Windows build không kill đúng process tree.
- **Detection**: Compile error on Windows (non-portable syscall)
- **Rust Mitigation**:
  - Unix: `nix::sys::signal::{kill, Sig::SIGTERM, Sig::SIGKILL}` cho negative PID (process group)
  - Windows: `std::process::Command::kill()` + Job Object API
  - `#[cfg(unix)]` / `#[cfg(windows)]` conditional compilation
  - Abstract `ProcessManager` trait with platform implementations
  - Timeout: tokio timer (cross-platform) không dùng signal
- **Test**: Test on macOS + Linux (full), Windows (basic kill)

### EC-015: PORT env var conflict with other services
- **Category**: System
- **Severity**: LOW
- **Description**: Looper daemon mặc định port 17310. Nếu port được cấu hình không available (already in use), server start fails. Go không có port auto-selection fallback.
- **Impact**: Daemon không start được.
- **Detection**: `bind: address already in use`
- **Rust Mitigation**:
  - Check port availability trước khi bind (attempt bind → if EADDRINUSE → error with suggestions)
  - PID file check trước (daemon already running?)
  - Config fallback: `server.port` có thể là 0 → random port
- **Test**: Test port conflict, port auto-recovery

---

## 3. Network Edge Cases

### EC-020: Network partition — heartbeat timeout
- **Category**: Network
- **Severity**: HIGH
- **Description**: Node sends heartbeat every 10s. Server marks node stale after 30s (lease TTL). If network partition (node alive but can't reach server), server expires node's coordinator lease. Node không biết mình bị expire → tiếp tục act như coordinator. Split-brain.
- **Impact**: Two nodes act as coordinator simultaneously → conflicting state changes (both triage same issues, both dispatch same work).
- **Detection**: Server expires lease, logs "lease expired for node X"
- **Rust Mitigation**:
  - Fencing token: mỗi lease operation check token version
  - Stale token → 412 Precondition Failed
  - Node-side: verify lease ownership before every action
  - Lease revalidation endpoint: server probes node URL để confirm node still alive
  - Configurable: `network.leaseTtlSeconds` (default 30)
- **Test**: Partition simulation (block heartbeat), verify lease handoff, verify fencing

### EC-021: Webhook duplicate delivery (at-least-once semantics)
- **Category**: Network
- **Severity**: MEDIUM
- **Description**: GitHub webhooks có at-least-once delivery (may deliver same event multiple times). Go code có dedup: delivery ID tracked in `deliveries` map với 1h TTL. Work key dedup: `workKey{ProjectID, Repo, ObjectType, Number, Branch}`. Nhưng TTL map in-memory, lost on restart.
- **Impact**: Duplicate queue items (same dedupe_key → unique constraint catch, but waste processing)
- **Detection**: Log "deliveryDeduped" count
- **Rust Mitigation**:
  - Persist delivery IDs in DB (not in-memory) với TTL
  - TTL cleanup background task (delete expired delivery records)
  - SQL unique constraint on queue_items(dedupe_key) WHERE status IN ('queued','running')
  - Still in-memory cache for hot dedup (fast path) + DB for persistence
- **Test**: Test duplicate webhook delivery, verify dedup across daemon restart

### EC-022: Rapid node join/leave/rejoin (flapping)
- **Category**: Network
- **Severity**: MEDIUM
- **Description**: Node joins network → leaves → rejoins with same name. Go code reactivates inactive node record (node_name UNIQUE COLLATE NOCASE). But new token generated each join. Old token invalidated.
- **Impact**: Any code still holding old node token gets 401 Unauthorized. Brief window where old token still cached.
- **Detection**: Server log "node X rejoined, reactivated"
- **Rust Mitigation**:
  - Token validation: check both active AND node_name match (not just token)
  - Grace period: old token valid for 5s after rejoin (avoid race)
  - Unique node_name constraint prevents duplicate
- **Test**: Rapid join/leave/rejoin cycle, verify token lifecycle

### EC-023: Webhook malformed payload
- **Category**: Network
- **Severity**: LOW
- **Description**: GitHub sends webhook with malformed JSON, missing headers (X-GitHub-Delivery, X-GitHub-Event), or invalid event type. Go handler parses body as `json.RawMessage` và validates headers.
- **Impact**: Invalid event ignored/dropped with error response.
- **Detection**: "webhook parse failed" log, 400 response
- **Rust Mitigation**:
  - `serde_json::from_reader` with error details
  - Validate required headers before parsing body
  - Unknown event types → respond 200 (GitHub will retry if 4xx/5xx)
  - Structured error response: `{"status": "error", "reason": "invalid event type"}`
- **Test**: Test with malformed JSON, missing headers, unknown events

### EC-024: SSE stream disconnect during long poll
- **Category**: Network
- **Severity**: LOW
- **Description**: CLI subscribes to SSE stream (`GET /v1/events`). If client disconnects (network, timeout), server continues sending to closed channel → panic in Go (send on closed channel) nếu không guard.
- **Impact**: Server-side crash if channel send not guarded
- **Detection**: n/a (crash)
- **Rust Mitigation**:
  - `tokio::sync::broadcast` hoặc `watch` channel pattern
  - Client disconnect: `select! { event = rx.recv() => ..., _ = client.closed() => break }`
  - Drop subscriber on disconnect (broadcast handles this via dropped receiver)
- **Test**: SSE subscribe → client drops → verify server continues running

---

## 4. Agent Execution Edge Cases

### EC-030: Agent binary not found on PATH
- **Category**: Agent
- **Severity**: MEDIUM
- **Description**: Agent binary (claude, codex, agent, opencode, hermes) không có trên PATH. Go executor: `exec.Command(binary, args...)` — khi `cmd.Start()` được gọi, `LookPath` fail với "executable file not found". Native resume fallback thử fresh spawn không resume flag, nhưng nếu binary vẫn missing → double-wrap error. Không có actionable guidance cho user.
- **Impact**: Run fails immediately, user sees opaque error.
- **Detection**: "start agent command: executable file not found"
- **Rust Mitigation**:
  - Pre-check: `which::which()` at startup → check tất cả configured vendors
  - Bootstrap: warn if agent binary not found (daemon still starts)
  - Runtime: clear error message "Claude Code binary not found. Install with: brew install claude-code"
  - Per-vendor known install commands:
    - claude-code: `brew install claude-code | npm install -g @anthropic-ai/claude-code`
    - codex: `brew install codex`
    - opencode: `brew install opencode`
    - cursor-cli: via Cursor IDE
    - hermes: `brew install hermes`
- **Test**: Test with missing binary, verify error message contains install instructions

### EC-031: Agent OOM-killed but LOOPER_RESULT marker present
- **Category**: Agent
- **Severity**: HIGH
- **Description**: Agent OOM-killed (SIGKILL) nhưng stdout buffer was flushed trước khi die, containing valid LOOPER_RESULT marker. Go executor: `finalStatus()` returns "failed" (exit code != 0). But `parseCompletion()` found valid marker but result discarded (line 514: if status not "completed", ParseStatus="missing"). Git changes may be partially written.
- **Impact**: False-negative: valid work discarded. Or false-positive: if exit code = 0 (SIGKILL can produce 0?), corrupted worktree treated as success.
- **Detection**: Exit signal=SIGKILL + marker present. Log warning "SIGKILL with valid marker — possible partial output"
- **Rust Mitigation**:
  - Priority: trust exit code over marker content. Exit non-zero → result discard.
  - Additional guard: check `wait.status().signal()` — if SIGKILL, always fail regardless of marker
  - Log warning: "SIGKILL — trust exit code over marker"
  - Git reconcile step will catch dirty state
- **Test**: fake-agent produces marker then SIGKILL, verify result treated as failed

### EC-032: Agent hangs (no output for long period)
- **Category**: Agent
- **Severity**: MEDIUM
- **Description**: Agent produces no stdout/stderr output. Go heartbeat timer fires: `idleTimeout` (default 10-15min depending on runner). Sends SIGTERM → 5s grace → SIGKILL. But agent might be in long computation and will eventually produce output.
- **Impact**: Agent killed during legitimately long computation.
- **Detection**: "idle timeout" log, timeout_type="idle"
- **Rust Mitigation**:
  - Same 2-tier timeout as Go: max_runtime + idle_timeout
  - Idle timeout default: 10min (planner/reviewer/fixer), 15min (worker)
  - Configurable: `agent.timeouts.*Seconds`
  - Potential improvement: adaptive timeout based on agent progress (soft reset if agent still seems active via CPU instead of just stdout)
  - Gradated kill: SIGTERM → wait 10s → check output → SIGKILL if still idle
- **Test**: fake-agent with delayed output, verify timeout fires correctly

### EC-033: Agent produces very large output (>256KB)
- **Category**: Agent
- **Severity**: MEDIUM
- **Description**: Agent produces output >256KB. Go executor caps in-memory buffer at `defaultMaxOutputBytes` (256KB). Output truncated. But persisted log file gets full output (>16MB max read back). Completion marker search scans full output then truncated buffer.
- **Impact**: Truncated output if marker appears after 256KB. False missing marker.
- **Detection**: "output truncated" log
- **Rust Mitigation**:
  - Same bounded buffer: `BoundedBuffer` (256KB in-memory) + full log file
  - Completion marker search: scan BOTH in-memory + persisted log
  - Marker in truncated region → still found (scan persisted log)
  - Max persisted log read: 16MB (configurable)
- **Test**: fake-agent produces 500KB output with late marker, verify marker found via log file

### EC-034: Native session ID extraction fails (unstructured output)
- **Category**: Agent
- **Severity**: MEDIUM
- **Description**: Agent outputs unstructured text from which `extractNativeSessionID()` tries to parse JSON keys (nativeSessionId, native_session_id, sessionId, etc.). If vendor changes output format, extraction fails → native resume not available next run.
- **Impact**: Fallback to fresh spawn (no resume). Extra agent time (wasted context).
- **Detection**: "failed to extract native session id" log, NativeResumeStatus="failed"
- **Rust Mitigation**:
  - Multiple parsing strategies: JSON parse → key:value regex → key=value regex
  - Write extracted session ID to agent log file (debugging)
  - Per-vendor extraction function (different patterns per vendor)
  - Fallback after failed extraction: fresh spawn (same as Go)
- **Test**: Test extraction with various output formats, verify fallback

### EC-035: Agent binary changed between runs (resume version mismatch)
- **Category**: Agent
- **Severity**: LOW
- **Description**: User upgrades agent binary between runs. Native session from old version incompatible with new version. Resume fails.
- **Impact**: Resume fails → fallback to fresh spawn.
- **Detection**: Agent stderr shows "session not found" or version mismatch
- **Rust Mitigation**:
  - Record agent binary SHA256 + version in AgentExecutionRecord metadata
  - On resume: if binary hash changed → skip native resume (fresh spawn)
  - Log "agent binary changed, skipping native resume"
- **Test**: Test resume with different binary version

### EC-036: Empty prompt sent to agent
- **Category**: Agent
- **Severity**: MEDIUM
- **Description**: Build prompt returns empty string. Go validates `input.Prompt` không được empty (line ~203). Nếu empty, Start() returns error before spawning process.
- **Impact**: Run fails immediately (hard error, not retryable).
- **Detection**: "prompt is required" validation error
- **Rust Mitigation**:
  - Same: validate prompt empty before spawn
  - Early return with `RunnerError::NonRetryable("empty prompt")`
  - This should never happen — internal logic bug
- **Test**: Test empty prompt path

---

## 5. Config Edge Cases

### EC-040: Empty config file
- **Category**: Config
- **Severity**: MEDIUM
- **Description**: Config file exists but is empty (0 bytes). Go decoder: JSON parser returns EOF error; TOML/TOML parser returns empty map (no error). PartialConfig with all Option::None. Merged with defaults → everything default.
- **Impact**: JSON: hard error (config load fails). TOML: silent all-defaults (may surprise user).
- **Detection**: JSON: "unexpected end of JSON input". TOML: no warning.
- **Rust Mitigation**:
  - TOML: detect 0-byte file → return PartialConfig (all None) + warning
  - JSON: detect 0-byte file → warn, don't error (treat as empty config)
  - Log warning: "config file at {path} is empty, using defaults"
- **Test**: Test empty TOML, empty JSON, non-existent config

### EC-041: Config file with unknown fields
- **Category**: Config
- **Severity**: MEDIUM
- **Description**: Config file contains fields not in schema (typo, future version, wrong format). Go JSON decoder uses `DisallowUnknownFields`. TOML/YAML decode to `map[string]any` → re-encode as JSON → unknown fields lost silently. Go normalizer does NOT warn about unknown TOML/YAML fields.
- **Impact**: User's config values silently ignored. Confusing behavior.
- **Detection**: No detection in Go (TOML/YAML). JSON: error with unknown field name.
- **Rust Mitigation**:
  - All formats: after deserialize → serialize back → compare with raw (detect lost fields)
  - Or: `serde(deny_unknown_fields)` on strict mode, warn-only on lenient
  - Log warning: "unknown config field: {path}" with suggestion (fuzzy match known fields)
- **Test**: Test with typo fields in all 3 formats

### EC-042: Mixed old schema + new schema (legacy compatibility)
- **Category**: Config
- **Severity**: HIGH
- **Description**: Go supports both old schema (top-level `reviewer` section) and new schema (`roles.reviewer`). `collect_mixed_schema_warnings()` detects both present. Normalization merges old fields into new location, preferring new values on conflict. This is CRITICAL for migration path.
- **Impact**: If old fields silently override new fields (or vice versa), config not what user expects.
- **Detection**: Log warning "mixed schema detected: reviewer section (old) + roles.reviewer (new)"
- **Rust Mitigation**:
  - Support both schema in PartialConfig (legacy fields marked deprecated)
  - Normalize: merge legacy fields into canonical location
  - Rule: new schema takes precedence over old schema on conflict
  - Log warnings for each legacy field used
- **Test**: Test old-only, new-only, mixed configs

### EC-043: HOME directory not writable
- **Category**: Config
- **Severity**: HIGH
- **Description**: `~/.looper/` directory can't be created or written. Default DB path, log dir, worktree root đều under HOME. Go bootstrap fail-fast: ensure runtime directories are writable. Daemon won't start.
- **Impact**: Daemon không start được.
- **Detection**: "directory {path} is not writable" error
- **Rust Mitigation**:
  - Same: fail-fast at bootstrap
  - Alternative: allow custom `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME` paths
  - Windows: APPDATA fallback
  - Clear error message: "Cannot create ~/.looper/. Set XDG_DATA_HOME to override."
- **Test**: Test with read-only HOME, test with XDG overrides

### EC-044: Config path with spaces / special characters
- **Category**: Config
- **Severity**: LOW
- **Description**: User specifies config path with spaces (e.g., `--config "/path/with spaces/config.toml"`). Go uses `os.Open()` — handles spaces fine. But path resolution chaining (cwd + relative path) có thể fail nếu cwd contains spaces.
- **Impact**: Config không tìm thấy.
- **Detection**: "config file not found" at specified path
- **Rust Mitigation**:
  - `PathBuf` handles spaces natively
  - Canonicalize path (std::fs::canonicalize) for logging/debugging
  - Error message: include actual attempted path
- **Test**: Test config path with spaces, unicode, special chars

---

## 6. Worktree Edge Cases

### EC-050: Stale git lock files (.lock)
- **Category**: Worktree
- **Severity**: MEDIUM
- **Description**: Git operations leave `.lock` files when killed mid-operation (e.g., `git fetch` killed by timeout). Git can't operate on locked repo. Go code: `runGitResult` retries fetch lock errors (2 retries, 50-100ms). Non-fetch operations fail immediately.
- **Impact**: Worktree operations fail with "cannot lock ref" / "Unable to create '.git/index.lock'"
- **Detection**: Git stderr: "fatal: Unable to create '<path>/.git/index.lock': File exists."
- **Rust Mitigation**:
  - Fetch lock: retry 3x with 100ms backoff (same as Go `fetchRefLockRetryDelays`)
  - Non-fetch: check lock file age — if >5min → warn + auto-remove? (careful design needed)
  - Or: `rm -f .git/index.lock` and retry — only for looper-created worktrees
  - Option: use `tempfile::TempDir`-based lock instead of git internal locks
- **Test**: Simulate lock file, test retry behavior, test auto-cleanup

### EC-051: Branch already exists on remote (force push fails)
- **Category**: Worktree
- **Severity**: HIGH
- **Description**: Fixer/Worker push to branch that exists on remote with diverged history. `git push --force-with-lease` checks expected SHA. If remote SHA differs (someone else pushed), push fails. Go returns `RemoteHeadChangedError`.
- **Impact**: Push fails, run enters retry or manual_intervention.
- **Detection**: "remote head changed" / "stale info" error
- **Rust Mitigation**:
  - `git2::PushOptions` with force + refspec
  - Verify `merge-base --is-ancestor` before push (same as Go)
  - On conflict: fetch actual remote SHA, offer user choice (override or abort)
  - Auto-resolution: if force push is allowed AND no critical changes lost → proceed
- **Test**: Simulate remote head change, test push conflict resolution

### EC-052: Concurrent worktree creation for same branch
- **Category**: Worktree
- **Severity**: MEDIUM
- **Description**: Two runners (planner + fixer?) try to create worktree for same branch concurrently. Git worktree add fails if worktree path already exists. Go code: first checks DB for existing record (RestoreWorktree), then checks `git worktree list`. Race: both pass checks → both try `git worktree add` → second fails.
- **Impact**: Second operation fails → retry → create new worktree DB record (duplicate?).
- **Detection**: "worktree already exists" from git
- **Rust Mitigation**:
  - DB-level lock: acquire advisory lock on worktree creation (INSERT OR IGNORE + SELECT)
  - Atomic: check-DB -> create-git -> upsert-DB (all in transaction)
  - On collision: retry with backoff, or use existing worktree
- **Test**: Concurrent worktree creation test, verify no duplicates

### EC-053: Protected branch enforcement
- **Category**: Worktree
- **Severity**: HIGH
- **Description**: User configures base_branch (e.g., "main") as protected. `AssertWritableBranch()` blocks worktree operations on protected branches. Base branch and starting point auto-added to protected list. But enforcement depends on config — user can override.
- **Impact**: Accidental push to main/production branch.
- **Detection**: "branch {branch} is protected" error
- **Rust Mitigation**:
  - Same: `assert_writable_branch()` at every mutation point
  - Default protected: main, master, develop, production
  - Configurable: `defaults.protectedBranches` (list)
  - Plus: GitHub branch protection API check (optional)
  - Plus: `git push` rejection even if looper allows (double safety)
- **Test**: Test with protected branch, verify push blocked

### EC-054: Stale worktree left on daemon crash
- **Category**: Worktree
- **Severity**: LOW
- **Description**: Daemon crashes while worktree is checked out → worktree orphaned on disk. Recovery pipeline phase 4 handles this: loop normalization, stale run reconcile.
- **Impact**: Orphaned worktrees consume disk space.
- **Detection**: Startup recovery logs "cleaned N orphaned worktrees"
- **Rust Mitigation**:
  - Recovery pipeline (module-recovery-infra): clean stale worktrees
  - WorktreeCleanup background loop: retention-based cleanup (default 7 days)
  - WorktreeRecord status "active" → "cleaned" on cleanup
- **Test**: Crash simulation, verify recovery cleans worktrees

---

## 7. Concurrency / Race Edge Cases

### EC-060: Scheduler + Webhook + CLI simultaneous DB writes
- **Category**: Concurrency
- **Severity**: MEDIUM
- **Description**: Scheduler tick writes claim updates. Webhook forwarder writes new queue items. CLI writes project/loop updates. All use same SQLite DB. WAL mode allows concurrent reads but write lock serializes.
- **Impact**: SQLITE_BUSY under load. Transient retries accumulate.
- **Detection**: "sqlite busy" errors in logs
- **Rust Mitigation**:
  - `deadpool-sqlite` with pool size = 4 (same as Go)
  - `busy_timeout = 5000`
  - WAL mode for read/write concurrency
  - Retry transient SQLITE_BUSY in critical paths
  - Consider: dedicated write connection for scheduler ticks?
- **Test**: Load test: 3 concurrent writers, verify no data loss

### EC-061: Startup race (two daemon instances)
- **Category**: Concurrency
- **Severity**: HIGH
- **Description**: User starts daemon twice. Or daemon restart fails to kill old process. Two daemon instances access same SQLite DB concurrently → corruption risk.
- **Impact**: SQLite corruption if both write simultaneously.
- **Detection**: Lock file collision on startup
- **Rust Mitigation**:
  - PID file: write on startup, check existing PID + process alive
  - File lock: `fs2::FileLock::try_lock_exclusive()` on `~/.looper/looperd.lock`
  - If lock held: read PID from file → if process dead → remove lock and retry
  - If process alive → error "looperd already running (PID: N)"
- **Test**: Test double-start, test stale lock recovery

### EC-062: Shutdown race — partial state persistence
- **Category**: Concurrency
- **Severity**: MEDIUM
- **Description**: Daemon receives SIGTERM mid-write (queue update, event log insert). Partial write → inconsistent state. Go shutdown: signal handler initiates graceful shutdown → server drain (1s) → runtime stop → scheduler stop → webhook stop → agent kill.
- **Impact**: Inconsistent DB state, lost events, orphaned runs.
- **Detection**: Recovery pipeline catches inconsistencies on next startup
- **Rust Mitigation**:
  - Shutdown ordering: signal → CancellationToken → API drain (1s) → scheduler stop → agent kill → webhook stop → storage close
  - Transactional writes: short transactions, no .await inside tx
  - Recovery pipeline (5 phases) handles inconsistencies on startup
- **Test**: SIGTERM during write operation, verify recovery

---

## 8. Lifecycle / Coordinator Edge Cases

### EC-070: Head SHA changes mid-review (force push)
- **Category**: Lifecycle
- **Severity**: HIGH
- **Description**: Reviewer discovers PR at head_sha=A. Agent starts review of diff at A. While agent reviews, author force-pushes head_sha=B. Reviewer submits review based on A → review may reference lines/code that no longer exist. GitHub rejects inline comments referencing old diff positions.
- **Impact**: Review inline comments fail (invalid position). Review body still posted but inline comments lost.
- **Detection**: GitHub API returns "outdated diff" error on comment submission
- **Rust Mitigation**:
  - Check head_sha before submit_review: if changed → re-discover PR, flag review as "review_stale"
  - Auto-recover: re-run review if head changed (if loop enabled)
  - Head_sha in review marker for tracking
  - ReviewMarker: store `head` field, compare on retry
- **Test**: Force push during review, verify auto-recovery

### EC-071: Auto-merge race with human intervention
- **Category**: Lifecycle
- **Severity**: MEDIUM
- **Description**: Reviewer enables auto-merge (via `gh pr merge --auto`). Human manually merges PR before auto-merge fires. Or human disables auto-merge temporarily. MergeWatch classifier detects `HumanDisabledAutoMerge` → stops watching. But what if human re-enables?
- **Impact**: MergeWatch stops watching → PR merged but no downstream action (worktree cleanup, issue close).
- **Detection**: "auto-merge disabled by human" log
- **Rust Mitigation**:
  - MergeWatch: watch for PR close/merge event directly (not just auto-merge state)
  - Webhook: `pull_request.closed` event → trigger merge cleanup
  - If auto-merge disabled → one final check: is PR merged? Yes → Merged action. No → HumanDisabledAutoMerge.
- **Test**: Auto-merge + human merge race, verify downstream actions fire

### EC-072: Duplicate webhook delivery triggers double discovery
- **Category**: Lifecycle
- **Severity**: MEDIUM
- **Description**: GitHub sends same webhook event twice (at-least-once). Both deliveries pass dedup check (first one creates work item, second sees duplicate key → coalesce instead of creating new). But if first delivery's item was already claimed and completed before second delivery arrives, the second may think it's a new event.
- **Impact**: Duplicate queue items, redundant processing.
- **Detection**: "delivery coalesced by dedupe_key" log
- **Rust Mitigation**:
  - Dedup: work key `{ProjectID, Repo, ObjectType, Number, Branch}` with unique partial index
  - Even if first completed, second can't create active duplicate (unique constraint)
  - Log warning if second delivery creates no-op
- **Test**: Duplicate webhook delivery with timing variations

---

## Appendix: Edge Case Summary

| ID | Category | Severity | Title |
|----|----------|----------|-------|
| EC-001 | SQLite | HIGH | SQLITE_BUSY on concurrent queue operations |
| EC-002 | SQLite | CRITICAL | Migration version mismatch on startup |
| EC-003 | SQLite | LOW | UUIDv4 / event ID collision |
| EC-004 | SQLite | MEDIUM | Large data (event_logs, runs) unbounded growth |
| EC-005 | SQLite | LOW | Unicode repo paths |
| EC-006 | SQLite | MEDIUM | Concurrent transaction deadlock |
| EC-010 | System | CRITICAL | Symlink traversal in worktree path |
| EC-011 | System | HIGH | Disk full during write operations |
| EC-012 | System | MEDIUM | Clock skew (NTP) |
| EC-013 | System | MEDIUM | Zombie processes |
| EC-014 | System | HIGH | Cross-platform signal differences |
| EC-015 | System | LOW | Port conflict |
| EC-020 | Network | HIGH | Network partition — heartbeat timeout / split-brain |
| EC-021 | Network | MEDIUM | Webhook duplicate delivery |
| EC-022 | Network | MEDIUM | Rapid node join/leave/rejoin |
| EC-023 | Network | LOW | Webhook malformed payload |
| EC-024 | Network | LOW | SSE disconnect during streaming |
| EC-030 | Agent | MEDIUM | Agent binary not found |
| EC-031 | Agent | HIGH | Agent OOM-killed with marker present |
| EC-032 | Agent | MEDIUM | Agent hangs (idle timeout) |
| EC-033 | Agent | MEDIUM | Large agent output (>256KB) |
| EC-034 | Agent | MEDIUM | Native session ID extraction failure |
| EC-035 | Agent | LOW | Agent binary version mismatch on resume |
| EC-036 | Agent | MEDIUM | Empty prompt |
| EC-040 | Config | MEDIUM | Empty config file |
| EC-041 | Config | MEDIUM | Unknown config fields (silently ignored) |
| EC-042 | Config | HIGH | Mixed old schema + new schema |
| EC-043 | Config | HIGH | HOME not writable |
| EC-044 | Config | LOW | Special characters in paths |
| EC-050 | Worktree | MEDIUM | Stale git lock files |
| EC-051 | Worktree | HIGH | Force push failure |
| EC-052 | Worktree | MEDIUM | Concurrent worktree creation |
| EC-053 | Worktree | HIGH | Protected branch bypass |
| EC-054 | Worktree | LOW | Stale worktree from crash |
| EC-060 | Concurrency | MEDIUM | Scheduler+webhook+CLI simultaneous write |
| EC-061 | Concurrency | HIGH | Two daemon instances |
| EC-062 | Concurrency | MEDIUM | Shutdown race — partial persistence |
| EC-070 | Lifecycle | HIGH | Head SHA changes mid-review |
| EC-071 | Lifecycle | MEDIUM | Auto-merge race with human |
| EC-072 | Lifecycle | MEDIUM | Duplicate webhook double discovery |

**Total: 40 edge cases** (3 CRITICAL, 12 HIGH, 20 MEDIUM, 5 LOW)
