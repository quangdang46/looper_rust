# Module Recovery Infra: Bootstrap, Shell Execution, Recovery Pipeline, Runtime, Notifications, WorktreeCleanup

## 1. Overview

Module nay chiu trach nhiem cho toan bo ha tang **bootstrapping, runtime lifecycle, recovery, notifications, va worktree cleanup** cua looperd. No la lop giao dien giua cau hinh (config), persistent storage, va cac loop runner (reviewer / worker / fixer / planner).

Khong giong cac module khac chi phuc vu mot loop type duy nhat, module nay la **shared infrastructure** ma tat ca cac loop type deu phu thuoc vao.

---

## 2. Bootstrap Sequence

Bootstrap la qua trinh khoi dong looperd tu trang thai `not started` den trang thai `ready`. No dien ra theo thu tu sau, va bat ky buoc nao that bai deu dung startup lai ngay lap tuc:

### 2.1 LoadConfig

Doc cau hinh tu CLI flags, environment variables, va file `~/.looper/config.json` (theo thu tu uu tien). Tra ve `LoadedFileConfig` bao gom:

- `config.Config` — cau hinh da duoc merge va validate
- `config.LoadFileMetadata` — metadata ve file config (duong dan, co ton tai khong, tool detection status)

### 2.2 ValidateToolPaths

Kiem tra cac tool path da duoc cau hinh (hoac phat hien tu dong) co tro den executable file ton tai hay khong:

- `tools.gitPath` — git binary
- `tools.ghPath` — gh (GitHub CLI) binary
- `tools.osascriptPath` — osascript binary (chi bat buoc neu `notifications.osascript.enabled = true`)

Voi moi tool path la absolute path, kiem tra:
1. File ton tai va khong phai directory
2. Co the `exec.LookPath` tim thay duoc

Neu bat ky check nao that bai, tra ve `ConfigValidationError` va dung startup.

### 2.3 EnsureDirs

Dam bao cac runtime directories ton tai va co the ghi duoc:

1. `daemon.logDir` — log directory. Tu dong tao neu chua ton tai. Kiem tra kha nang ghi bang cach tao temporary file.
2. Parent directory cua `storage.dbPath` — SQLite database directory. Tu dong tao neu chua ton tai.
3. `daemon.workingDirectory` — working directory cho runtime. Kiem tra kha nang ghi nhung khong tu dong tao.

`ensureWritableDirectory` su dung co che tao temporary file (`os.CreateTemp`) de kiem tra quyen ghi, sau do cleanup.

### 2.4 CreateLogger

Tao Logger instance:

- Log ra file `looperd.log` trong `daemon.logDir`
- Log ra stdout (cho log level info/debug) va stderr (cho log level warn/error)
- Dinh dang JSON: `{"ts": "2026-06-21T10:00:00.000+07:00", "level": "info", "message": "...", "context": {...}}`
- Hon 4 log levels: `debug`, `info`, `warn`, `error`
- Co che log rotation: gioi han kich thuoc (`MaxSizeMB`), toi da `MaxFiles` file archive
- Thread-safe (su dung `sync.Mutex`)
- Co che filter theo log level: neu priority cua entry < priority cua configured level, entry bi bo qua
- Timestamp format: `2006-01-02T15:04:05.000-07:00` (local time)

### 2.5 StartRuntime

Khoi tao Runtime struct, goi `Start(ctx)`. Neu `Start` tra ve error, startup that bai.

`Start` bao gom:

1. **Daemon lock acquisition** (neu webhook enabled): acquire file lock de dam bao chi mot looperd instance chay
2. **Open SQLite coordinator**: mo database connection, chay auto-migration neu `autoMigrateOnStartup = true`
3. **Build dependency graph**: tao `Repositories`, `GitGateway`, `GitHubGateway`, `ProjectService`, `LoopService`, `RunService`
4. **Project sync**: goi `SyncConfiguredProjects` de dong bo cac project tu config vao database
5. **Assembly**: gan cac service vao `Runtime.services` field
6. **Default scheduler handlers**: neu khong co custom scheduler tick, build default scheduler handlers (tick + claim + webhook forwarder)
7. **Network manager**: khoi tao `networkclient.Manager`

Neu `deferRecovery = true`, tra ve ngay sau step 7. Neu khong, goi `CompleteStartup`.

### 2.6 CompleteStartup

Day la pha thu hai cua startup, noi tat ca recovery va subsystem starting dien ra:

1. **Validate coordinator dependency gates**: neu project co `coordinator.dependencies.enabled`, kiem tra GitHub API availability
2. **Run recovery pipeline** (`runRecoveryPipeline`): thuc hien 5-phase recovery
3. **Append started event**: ghi event `looperd.started` vao event log
4. **Start network manager**: khoi tao network connectivity
5. **Start webhook subsystem**: listen cho incoming webhooks
6. **Start scheduler loop**: bat dau scheduler vong lap poll
7. **Start worktree cleanup loop**: neu `worktreeCleanup.enabled`
8. **Start deferred reviewer recovery**: goi `runDeferredReviewerRecovery` trong background goroutine

Neu bat ky buoc nao that bai, `startupReadyErr` duoc set va `Start` tra ve error.

### 2.7 WaitForShutdown

Sau khi Bootstrap hoan tat, neu `options.WaitForShutdown = true`:

1. Ta o goroutine lang nghe `SIGINT` va `SIGTERM`
2. Khi nhan duoc signal, goi `runtime.Stop(reason)` va cho goroutine ket thuc
3. `runtime.WaitForShutdown()` blocking cho den khi `shutdownCh` duoc dong

---

## 3. Shell Execution

He thong shell execution la co che de looperd goi agent CLI (Claude Code, Codex, OpenCode, Cursor, Gemini, Hermes). No dam bao:

### 3.1 Process Group Isolation

- Su dung `exec.Command` de tao subprocess
- `SysProcAttr.Setpgid = true`: tao process group rieng
- Khi kill, dung `syscall.Kill(-pid, signal)` de kill toan bo process group
- Giam sat `ESRCH` de phat hien process da chet truoc khi kill

### 3.2 Timeout voi SIGTERM -> SIGKILL Escalation

Co che timeout hai giai doan:

```
SIGTERM signal process group
    |
    +-- GracePeriod (default 5 giay)
    |       |
    |       +-- Process tu ket thuc -> success
    |       +-- Khong tu ket thuc -> SIGKILL toan bo process group
    |
    +-- Neu `gracefulShutdown = 0`, dung default 5 giay
```

Co hai loai timeout:

- **Max runtime timeout**: gioi han thoi gian tuyet doi cua agent execution (`input.Timeout`)
- **Idle heartbeat timeout**: phat hien inactivity. Khi khong co output nao trong `heartbeatTimeout`, tinh la idle va bat dau termination sequence

### 3.3 Bounded Output Capture

- Max output bytes: `256KB` (default). Co the cau hinh qua `input.MaxOutputBytes`
- Co che `appendTailBounded`: gio lai phan cuoi cung cua output, khong vuot qua threshold
- Output duoc ghi vao persisted log files (`stdout.log` va `stderr.log`) trong `logDir/loops/{loopID}/{runID}/`
- Persisted log co gioi han doc lai: `16MB` (trong qua trinh `resolveOutputLogs`)
- Neu persist log write that bai (`persistedLogWriteFailed`), fallback ve in-memory buffers

### 3.4 Stream Capture va Heartbeats

- `streamCapture` wrapper: moi chunk output tu subprocess duoc capture va `onChunk` callback
- `onOutput` handling:
  1. Tang heartbeat count
  2. Cap nhat `lastHeartbeatAt` va `lastOutputAt`
  3. Persist stdout/stderr to files
  4. Append tail-bounded output buffer
  5. Extract native session ID tu output (JSON hoac key=value format)
  6. Goi `persistStatus` de cap nhat `AgentExecutionRecord` trong database
  7. Goi `bumpRunHeartbeat` de cap nhat `RunRecord.lastHeartbeatAt`

### 3.5 Command Spawning

- `resolveCommand`: chon binary dua tren vendor
- `resolveArgs`: xay dung argument list dua tren vendor va parameters
- `buildCommandEnv`: xay dung environment variables:
  - Start tu `os.Environ()`
  - Merge voi `config.Env` va `input.Env`
  - Loai bo unsafe git environment keys (`GIT_DIR`, `GIT_WORK_TREE`, etc.)
  - Them `LOOPER_PROMPT` va `LOOPER_COMPLETION_MARKER`
- Unsafe git environment keys duoc strip de tranh agent co the lam hong git state cua looperd

### 3.6 Native Resume

- `resolveNativeResume`: kiem tra xem co the resume session tu agent execution truoc khong
- Agent vendors ho tro: Claude Code (`--resume`), Codex (`resume`), OpenCode (`--session`), Cursor CLI (`--resume`)
- Neu native resume that bai (stderr chua "resume failed" pattern), fallback ve checkpoint restart: spawn lai agent tu dau
- Fallback duoc ghi nhan trong `AgentExecutionRecord.nativeResumeStatus = "fallback_started"` va `"fallback_completed"`

### 3.7 Completion Marker

- Agent output duoc quet de tim `completionMarker` (JSON prefix)
- Parse completion marker de trich xuat: summary, artifacts, changedFiles, commits, git pr lifecycle
- Neu completion marker khong tim thay: `parseStatus = "missing"`
- Neu completion marker co invalid JSON: `parseStatus = "invalid_json"`
- Template completion markers (chi co `summary`: `<one-sentence summary>`) bi bo qua

### 3.8 Run() Ket Qua Tra Ve (`Result`)

```
Result {
    Status:                    // "completed", "failed", "timeout", "killed"
    Summary:                   // Human-readable summary
    Stdout:                    // Bounded stdout text
    Stderr:                    // Bounded stderr text
    ParseStatus:               // "parsed", "missing", "invalid_json"
    CompletionSignal:          // Completion marker prefix
    Artifacts:                 // Lists of artifacts
    ChangedFiles:              // Lists of changed files
    Commits:                   // Lists of git commits
    Lifecycle:                 // Git PR lifecycle state
    HeartbeatCount:            // Number of heartbeats
    TimeoutType:               // "max_runtime" or "idle"
    ConfiguredIdleTimeoutSeconds
    ConfiguredMaxRuntimeSeconds
    ElapsedRuntimeSeconds
    LastProgressAt
    PID                        // Process ID
}
```

### 3.9 Agent Execution Persistence

Moi agent execution duoc persist vao `AgentExecutionRecord` trong SQLite:

- **Trong khi chay**: `persistStatus` ghi status, heartbeat, output JSON
- **Khi ket thuc**: `persistFinal` ghi ket qua cuoi cung, summary, error message, parse status, completion signal
- Event log entries: `agent.invoked`, `agent.completed`, `agent.idle_timeout`, `agent.max_runtime_timeout`, `agent.killed`, `agent.native_resume_fallback_started`

---

## 4. Recovery Pipeline (5 Phase)

Khi `CompleteStartup` duoc goi (hoac trong `ReconcileStaleRunningRuns` mode live/manual), `runRecoveryPipeline` thuc hien 5 phase recovery. Day la co che dam bao looperd co the khoi dong lai an toan sau crash hoac restart.

### 4.1 Phase 1: Orphan Agent Cleanup

**Muc dich**: Tim va kill cac agent subprocess con song sau looperd crash truoc do.

**Thuat toan**:

1. Goi `AgentExecutions.ListActive(ctx)` de lay tat ca execution records co `status = "running"`
2. Voi moi execution:
   - Bo qua neu `PID = nil` hoac `PID <= 0`
   - Goi `executionMatchesProcess(execution, pid)` de kiem tra:
     - Doc `/proc/{pid}/cmdline` (hoac `ps -p {pid} -o command=`) de lay command line
     - So sanh command line voi `execution.CommandJSON` (tung args mot)
     - Tra ve `(matches: bool, running: bool, err: error)`
   - **Neu running + matches**: PID nay la agent execution.

     `signalProcessGroup(pid, SIGTERM)` -> cho 5s -> SIGKILL.
     Goi `markRecoveredExecutionTerminal` de cap nhat status thanh `"killed"`

   - **Neu running + !matches**: process khac dang chay voi PID do. Day la uncertain state:
     - Ghi vao `uncertainAgentRunIDs` va `uncertainExecutionIDs`
     - Ghi event `looperd.recovery.uncertain_process_identity`
     - Khong kill, khong modify execution record
   - **Neu !running**: process da tu ket thuc. Goi `markRecoveredExecutionTerminal` de cap nhat status

3. Ghi event `looperd.recovery.orphan_agent_cleanup` cho moi execution bi kill

**Output**: `RecoveryOrphanAgentCleanup`: `{ attempted: true, cleanedCount: N }`

### 4.2 Phase 2: Expired Lock Release

**Muc dich**: Giai phong tat ca locks het han trong database.

**Thuat toan**:

1. Goi `Locks.ListExpired(ctx, nowISO)` de lay danh sach lock co `expiresAt < now`
2. Voi moi expired lock:
   - Goi `Locks.Release(ctx, lock.Key)`
   - Ghi event `looperd.recovery.lock_released`: owner, expiredAt, recoveredAt

**Output**: `ExpiredLocksReleased: N`

### 4.3 Phase 3: Stale Run Reconciliation

**Muc dich**: Phat hien va interrupt cac `RunRecord` con o trang thai `running` nhung thuc te khong con hoat dong.

**Co che phat hien stale run**:

- **Startup mode**: moi `running` run duoc xem la candidate (cannot be sure about liveness after crash)
  - Neu run la latestRun cua loop va co active execution: goi `verifyRunExecutionLiveness`
    - Neu `uncertain`: skip, ghi event
    - Neu `live`: khong interrupt
    - Neu `dead`: interrupt
  - Neu run khong phai latestRun hoac khong co active execution: interrupt
- **Live/manual mode**: chi candidate khi:
  1. `latestRun != nil` (co run gan nhat)
  2. `!runHeartbeatIsRecent(run, now, 30 minutes)`: heartbeat > 30 phut thi xem la stale
  3. Co active execution, hoac latestRun.ID == run.ID va step co agent backing

**Khi phat hien stale run**:

1. `interruptRecoveryRun`: cap nhat run status -> `"interrupted"`, set `endedAt`, `errorMessage`
2. Cleanup executions (kill, mark terminal, ghi event)
3. `repairStaleRunQueueState`: requeue hoac normalize loop status dua tren queue state
   - Neu latest queue item la `manual_intervention`: normalize loop status
   - Neu latest queue item la `running`: requeue queue item va set loop -> `queued`
   - Neu `shouldRequeueLoop` (loop `running/queued`, latestRun terminated, khong co active agent): requeue loop
4. `repairInterruptedLoopQueueIfNeeded`: cho cac loop khong co active run hoac queue item, normalize ve terminal status

**Heartbeat threshold**: 30 phut. Run co `lastHeartbeatAt` trong 30 phut gan day duoc xem la con song (khong bi interrupt trong live/manual mode).

**Output**: `StaleRunReconcileSummary`: candidateRuns, interruptedRuns, loopsRequeued, queueItemsRequeued, queueItemsCancelled, cleanedExecutions, skippedUncertainRuns, eventsWritten

### 4.4 Phase 4: Loop State Normalization

**Muc dich**: Dam bao tat ca loop records co trang thai nhat quan voi run va queue state. Day la buoc "don dep" cuoi cung cua recovery.

**Thuat toan**:

1. Duyet qua tat ca loops
2. Bo qua loop da duoc requeued trong Phase 3
3. Kiem tra `normalizeTerminalReviewerLoopForRecovery`:
   - Neu loop status la `running`/`queued`/`paused`/`waiting`/`idle` nhung latest run da terminated -> cap nhat loop status dua tren latest queue item
4. Cho cac loop `manual_intervention` con status `running`/`queued`:
   - Neu queue status la `manual_intervention`: set loop status -> `paused`, xoa `nextRunAt`
   - Neu queue status la `running`: requeue queue item, set loop -> `queued`
5. Cho cac loop `running`/`queued` khong co active agent va latest run terminated:
   - `shouldRequeueLoop`: requeue loop + queue item
   - Ghi event `looperd.recovery.loop_requeued`
6. Cho cac loop con status `queued` nhung khong co queue item tuong ung:
   - `normalizeStaleQueuedLoopStatus`: set loop status dua tren latest run status
   - VD: latest run la `failed` -> loop -> `failed`; latest run la `completed` -> loop -> `idle`

### 4.5 Phase 5: Reviewer Auto-Recovery

**Muc dich**: Tu dong recover cac failed reviewer loops co the retry duoc.

**Quy trinh**:

1. Xac dinh recovery policy cho project:
   - `recoveryMode`: "none" (default) hoac "auto"
   - `enabled`: bool
   - `maxRetries`: int
   - `resetAttempts`: bool
   - `currentLogin`: string (gh auth login)
2. Kiem tra `shouldAutoRecoverFailedReviewerLoop`:
   - Loop status = `failed` (hoac `running`/`queued` da normalized -> `failed`)
   - Co recovery policy enabled
   - Queue item co status = `failed` va `lastErrorKind` la retryable (hoac enhanced transient classification)
   - Khong vuot qua `maxRetries`
3. Neu can re-authentication:
   - `reviewerRecoveryNeedsFreshLogin`: kiem tra login status
   - `runDeferredReviewerRecovery`: background goroutine cho login timeout
   - Sau khi co login, kiem tra va requeue lai
4. Goi `requeueFailedReviewerQueueItemForRecovery`:
   - Tao queue item moi
   - Neu `resetAttempts = true`, set `attempt = 1`
   - Copy `lastErrorKind` cho observability
5. Goi `autoRecoveredReviewerLoop`:
   - Set loop status -> `queued`
   - Set `nextRunAt = now`
   - Cap nhat `lastRunAt`
6. Ghi event `looperd.recovery.reviewer_auto_recovered`

**Co che defer**:
- Neu looperd can `gh auth status --min-status` de xac thuc login, va no co timeout 3 giay
- Neu timeout, reviewer recovery duoc deferred sang background goroutine
- Background goroutine cho login thanh cong, sau do thu lai recovery

**Output**: `RecoverySummary`: looperd.recovery.completed event voi day du counters

---

## 5. Runtime Assembly

### 5.1 Runtime Struct

`Runtime` la struct trung tam chua toan bo trang thai song cua looperd:

```
Runtime {
    config              // Config (immutable after start)
    logger              // Logger instance
    now                 // Time source (injectable for testing)

    // Storage
    services {          // Services() returns thread-safe snapshot
        Coordinator     // SQLite coordinator
        Repositories    // Storage repositories
        Projects        // Project service
        Loops           // Loop service
        Runs            // Run service
        ActiveExecutions // Active execution registry
    }

    // Subsystems
    githubGateway       // GitHub API gateway
    webhook             // Webhook runtime
    webhookForwarder    // Webhook delivery forwarder
    networkManager       // Network connectivity manager

    // Lifecycle
    startedAt           // Timestamp when start completed
    recovery            // RecoverySummary from last recovery
    stopped             // Flag: da stop chua?
    shutdownCh          // Channel: dong khi stop hoan tat
    startupOnce         // sync.Once cho start
    shutdownOnce        // sync.Once cho stop

    // Scheduler
    schedulerStop       // Channel: stop scheduler loop
    schedulerDone       // Channel: scheduler loop da exit
    schedulerWake       // Channel: wake scheduler immediately
    schedulerCancel     // Context cancel func
    schedulerTasks      // Tracker for async scheduler goroutines

    // Worktree cleanup
    worktreeCleanupStop
    worktreeCleanupDone
    worktreeCleanupCancel
    worktreeCleanupRunning
    worktreeCleanupStatus

    // Reviewer recovery
    recoveryCancel      // Background recovery goroutine
    recoveryDone
}
```

### 5.2 Services Accessor

`Services()` tra ve thread-safe snapshot cua service layer:

- Su dung `sync.RWMutex.RLock()` de doc
- Tra ve `Services` struct **copy** (khong phai pointer reference)
- Dung de dam bao cac loop runner khong bi anh huong khi runtime dang stop

### 5.3 CompleteStartup

`CompleteStartup(ctx)` la pha thu hai cua startup, duoc goi:

- Tu dong tu `start()` (neu `deferRecovery = false`)
- Explicitly boi caller (neu `deferRecovery = true`)

**Sequencing**:

```
1. Validate coordinator dependency gates
2. Run recovery pipeline
3. Append looperd.started event
4. Start network manager
5. Start webhook runtime
6. Start scheduler loop (neu agent vendor duoc cau hinh)
7. Start worktree cleanup loop (neu enabled)
8. Start deferred reviewer recovery (background)
```

`startupReadyOnce` dam bao `CompleteStartup` chi chay mot lan. Neu runtime da stopped, tra ve error.

### 5.4 Stop(reason)

`Stop(reason)` la co che graceful shutdown:

```
1. Logger: "looperd runtime stopping"
2. Stop deferred reviewer recovery goroutine
3. Stop worktree cleanup loop
4. Stop scheduler loop
5. Lock mutex, set stopped = true
6. Append looperd.stopped event
7. Clear services (set to zero value)
8. Close webhook forwarder
9. Stop network manager
10. Close SQLite coordinator
11. Close shutdownCh (giai phong WaitForShutdown)
12. Logger: "looperd runtime stopped"
```

`shutdownTimeout` bao ve viec stop khong bi treo mai:

- Neu scheduler loop khong thoat trong timeout, log warning va tiep tuc
- Neu worktree cleanup loop khong thoat trong timeout, log warning va tiep tuc
- Default: `config.Daemon.ShutdownTimeoutMS`, fallback 1 giay

### 5.5 WaitForShutdown

`WaitForShutdown()` blocking cho den khi `shutdownCh` duoc dong:

- Duoc goi tu `Bootstrap` sau `StartRuntime`
- `shutdownCh` chi dong trong `Stop()` (sau khi cleanup xong)
- Dam bao toan bo lifecycle: start -> run -> signal -> stop -> exit

---

## 6. Notifications

He thong notifications cung cap kha nang thong bao cho nguoi dung ve cac su kien trong looperd.

### 6.1 Kien truc Gateway

- **Gateway abstraction**: `notifications Gateway` interface
  - `Send(Notification) error`
  - `Throttle(key string, interval time.Duration) bool`
- **Multiple backends**:
  - `DatabaseBackend`: ghi notification vao `event_logs`
  - `OsascriptBackend`: goi macOS notification qua AppleScript (`display notification`)

### 6.2 Database Backend (In-App)

- Ghi notification records vao `event_logs` table
- Event types: `notification.sent`, `notification.failed`
- Fields: `entityType`, `entityID`, `payloadJSON` chua message
- Cho phep CLI va API query lich su notifications

### 6.3 Osascript AppleScript Backend

- Chi active khi `notifications.osascript.enabled = true`
- Goi `osascript -e 'display notification "message" with title "Looper" subtitle "..."'`
- Su dung `tools.osascriptPath` neu duoc cau hinh, neu khong tu dong phat hien tu PATH
- Fail silently neu osascript khong co san (khong block startup, chi log warning)
- Chi gui notification cho cac su kien quan trong:
  - Loop failure can manual intervention
  - Agent timeout / killed
  - PR review completed (configurable)

### 6.4 Throttling

- De tranh spam notifications trong thoi gian ngan
- Co che `Throttle(key, interval)`: notification chi duoc gui mot lan trong moi `interval` cho cung mot `key`
- Su dung in-memory map de track last sent time
- Keys: `"loop_{loopID}_failure"`, `"agent_{executionID}_timeout"`, etc.
- Throttle duoc auto-clean khi runtime stop

---

## 7. WorktreeCleanup

WorktreeCleanup la background process tu dong don dep cac git worktree khong con su dung.

### 7.1 Kien truc

Gom hai thanh phan:

- **`worktreecleanup.Service.Plan()`**: Plan logic (scheduling, side-effect free)
- **`worktreecleanup.Run()`**: Execution logic (actual git operations)

### 7.2 Plan() — Decision Engine

`Plan(ctx)` quet toan bo worktree records trong database va quyet dinh worktree nao co the clean:

**Input**: worktree records, loop records, run records, queue items

**Decision flow cho moi worktree**:

```
1. Scan worktree records:
   - Duyet `Worktrees.ListActive()`
   - Voi moi worktree, khoi tao `candidateState`

2. Cross-reference voi loops:
   - Duyet `Loops.List()`
   - Neu loop `metadataJSON` hoac `worktreePath`/`branch` match worktree:
     - Them reference (`kind: "loop"`)
     - Cap nhat `lastUsedAt` tu loop.UpdatedAt, loop.LastRunAt
     - Neu loop.status la protected (`idle/queued/running/waiting/paused/failed/interrupted`):
       = BLOCK: "referenced by protected loop status {status}"

3. Cross-reference voi runs:
   - Duyet `Runs.List()`
   - Neu run `checkpointJSON` match worktree:
     - Them reference (`kind: "run"`)
     - Cap nhat `lastUsedAt` tu run.UpdatedAt, run.StartedAt, run.EndedAt
     - Neu run.status = "running":
       = BLOCK: "referenced by running run"

4. Cross-reference voi queue items:
   - Duyet `Queue.List()` -> chi xet `status = queued/running`
   - Neu queue item match worktree:
     - Them reference (`kind: "queue"`)
     - Cap nhat `lastUsedAt`
     = BLOCK: "referenced by active queue item"

5. Determine action cho moi worktree:
   - Khong co reference -> ORPHAN
     - Neu `includeOrphans = false`: SKIP "orphan and includeOrphans=false"
   - Parse fail (checkpoint JSON khong parse duoc) -> SKIP "checkpoint parse failure"
   - Blocked (co active loop/run/queue) -> SKIP
   - `lastUsedAt` trong retention window -> SKIP "within retention window"
   - Dat `maxPerTick` limit:
     - Vuot qua `maxPerTick`: SKIP "maxPerTick limit reached"
   - WORKTREE READY FOR CLEAN -> WOULD_CLEAN
```

**Output**: `PlanResult`:
```
Summary {
    Scanned:    total worktrees
    Candidates: eligible worktrees
    WouldClean: up to MaxPerTick
    Skipped:    due to blocks/retention/limits
    Failed:     parse failures
    Orphans:    orphan worktrees
}
Decisions []Decision {
    Worktree:   worktree record
    Action:     "would_clean" | "skipped"
    Reason:     explanation
    LastUsedAt: timestamp
    Orphan:     boolean
    References: []{Kind, ID, Status}
}
```

### 7.3 Run() — Execution Engine

`Run(ctx, options)` nhan `PlanResult` va thuc hien cleanup:

```
Cho moi decision:
1. Xac dinh `project` tu config
2. `worktreeRootForProject`: project.WorktreeRoot || default path
3. Neu `decision.Action != ActionWouldClean`: SKIP
4. `inspectCandidate`:
   a. Da cleaned? -> SKIP "already_cleaned"
   b. Worktree safety validation (path security check)? -> SKIP "unsafe_path"
   c. Worktree path ton tai? -> SKIP/ERROR
   d. Git worktree sach? (IsWorktreeClean) -> SKIP "dirty_git_status"
   e. -> Action = "clean", Reason = "terminal_clean"
5. Neu `dryRun = true`: SKIP "dry_run"
6. Clean:
   - Goi `Git.CleanupWorktree(input)`:
     - `git worktree remove {path}`
     - `git branch -D {branch}` (neu la temp branch)
   - Update worktree record: status = "cleaned", cleanedAt = now
   - Ghi event: `worktree.cleanup.cleaned`
```

### 7.4 Retention Window

`retentionCutoff = now - (RetentionDays * 24 hours)`

- `lastUsedAt` (max cua: worktree.UpdatedAt, worktree.CreatedAt, loop.UpdatedAt, loop.LastRunAt, run.UpdatedAt, run.StartedAt, run.EndedAt)
- Neu `lastUsedAt > retentionCutoff` -> worktree con trong retention window -> SKIP
- Default `RetentionDays` = 7 (configurable qua `daemon.worktreeCleanup.retentionDays`)

### 7.5 Loop/Run/Queue Checks

**Loop check**:
- Duyet loops list, phan tich `metadataJSON` de lay `worktreeRef`
- `matchesRef`: match bang ID > path > branch (+ projectID)
- Protected statuses: `idle/queued/running/waiting/paused/failed/interrupted`

**Run check**:
- Duyet runs list, phan tich `checkpointJSON` de lay `worktreeRef`
- Filter bo sung tu loop metadata
- `run.status = running` -> absolute block

**Queue check**:
- Chi xet `status = queued/running`
- Phan tich `payloadJSON` cua queue item
- Neu queue item co LoopID, lay loop metadata de tim worktreeRef bo sung

**Dirty worktree protection**:
- `inspectCandidate` goi `git.IsWorktreeClean(path)` de kiem tra
- Neu dirty -> SKIP "dirty_git_status"
- Khong bao gio tu dong `git reset --hard` hoac xoa untracked files

### 7.6 MaxPerTick

- Gioi han so worktree clean trong moi tick
- Default: 10
- Configurable qua `daemon.worktreeCleanup.maxPerTick`
- Khi `wouldClean >= MaxPerTick`, cac worktree con lai duoc SKIP "maxPerTick limit reached"
- Dam bao worktree cleanup khong block runtime startup qua lau hoac tao CPU spike

### 7.7 Worktree Safety Validation

Truoc khi thuc hien bat ky operation nao tren worktree path:

1. Path security: dam bao `worktreePath` nam trong `worktreeRoot` (khong tro ra ngoai)
2. Khong cho phep clean worktree cua:
   - `baseBranch` (protected branch)
   - Main repository checkout
3. Neu path validation that bai -> SKIP "unsafe_worktree_path" ghi ro ly do

### 7.8 Worktree Cleanup Loop trong Runtime

`Runtime` quan ly worktree cleanup loop:

```
startWorktreeCleanupLoop():
    initialDelay = config (default 1 minute) -> doi -> tick
    interval = config.WorktreeCleanup.Interval (default 1 hour)

executeWorktreeCleanupPass():
    1. Check running flag (khong cho overlap)
    2. Build git gateway
    3. Goi WorktreeCleanup.Run() hoac Run_P lan
    4. Cap nhat worktreeCleanupStatus
    5. Ghi events: worktree.cleanup.started, completed, failed, skipped, cleaned

stopWorktreeCleanupLoop():
    Cancel context -> close stopCh -> cho doneCh timeout
```

`WorktreeCleanupStatus` hien thi trang thai hien tai:
- enabled, dryRun, lastStartedAt, lastCompletedAt, lastStatus
- scanned, candidates, cleaned, skipped, failed
- lastError

---

## 8. Edge Cases va Invariants

### 8.1 Bootstrap

- `daemon.workingDirectory` khong ton tai -> warning (tu dong tao khong bat buoc)
- DB path parent directory khong ghi duoc -> startup FAIL
- Tool path khong phai executable -> startup FAIL
- Config validation error -> startup FAIL (khong bao gio tu dong fix)

### 8.2 Agent Execution

- CommandStart that bai + native resume attempt that bai -> fallback checkpoint restart
- Agent stderr rong + exit code != 0 -> su dung `waitErr.Error()` lam stderr
- `NativeResumeStatus "failed"` duoc persist de log cho debug
- Max output exceed -> chi gio lai 256KB cuoi cung
- Agent spawn khong co prompt -> tra ve error ngay

### 8.3 Recovery Pipeline

- PID da duoc reuse cho process khac -> `!matches && running` -> uncertain, khong kill
- Uncertain execution -> khong block recovery, chi log va tiep tuc
- Stale run co heartbeat recent -> bi bo qua trong live mode
- Loop `running/queued` nhung khong co queue item -> normalized ve terminal status
- `manual_intervention` queue item -> loop status duoc normalize ve `paused`
- `shouldRequeueLoop` chi requeue neu: loop != terminated, latestRun terminated, khong co active agent execution
- Neu loop da `paused` hoac `failed` nhung requeue that bai -> ghi log warning, khong block recovery

### 8.4 WorktreeCleanup

- Worktree record `status = "cleaned"` -> bo qua, khong clean lai
- Worktree path khong con ton tai tren disk -> clean record + ghi event
- Project archive -> skip worktree cua project do
- Loop metadataJSON parse fail -> danh dau parseFailed (block clean) cho worktree matching project id
- WorktreePath tro ra ngoai worktreeRoot -> block "unsafe_path"
- Dirty git status -> block "dirty_git_status" (khong bao gio tu dong reset)
- Orphan worktree (khong reference nao) -> chi clean neu `includeOrphans = true`
- `maxPerTick` gioi han -> worktree cleanup khong bao gio clean qua nhieu trong mot tick
