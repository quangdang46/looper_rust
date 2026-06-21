# Module 10: looper-cli + looperd — Rust Port Spec

> Derived from:
> - `cmd/looper/main.go` (64 lines) — CLI entry point
> - `cmd/looperd/main.go` (744 lines) — Daemon entry point
> - `internal/cliapp/app.go` (880 lines) — Root cobra command tree
> - `internal/cliapp/daemon_client.go` (161 lines) — API client
> - `internal/cliapp/daemon_runtime.go` (1496 lines) — Daemon start/stop/restart
> - `internal/cliapp/daemon_install.go` (320 lines) — daemon install
> - `internal/cliapp/daemon_supervision.go` (552 lines) — launchd lifecycle
> - `internal/cliapp/download.go` (175 lines) — Binary download/extraction
> - `internal/cliapp/network_commands.go` (555 lines) — Network commands
> - `internal/cliapp/upgrade.go` (1588 lines) — Auto-upgrade
> - `internal/cliapp/config_commands.go` (1666 lines) — Config get/set/unset
> - `internal/cliapp/json_output.go` (992 lines) — Output formatters
> - `internal/cliapp/human_output.go` (80 lines) — Human-friendly output
> - `internal/cliapp/feedback.go` (159 lines) — Submit feedback
> - `internal/cliapp/prompt_commands.go` — Prompt preview
> - `internal/cliapp/queue_commands.go` — Queue inspection
> - `internal/cliapp/worktree_commands.go` — Worktree cleanup
> - `internal/cliapp/loop_diagnostics_commands.go` — Loop inspect/failures
> - `internal/cliapp/run_stats_commands.go` — Run stats
> - `internal/cliapp/netadmin_commands.go` (231 lines) — Netadmin commands
> - `internal/cliapp/review_submit.go` — Review submit
> - `internal/cliapp/takeover.go` — Takeover commands
> - `internal/cliapp/webhook_commands.go` — Webhook commands
> - `internal/cliapp/labels.go` — Label commands
> - `internal/cliapp/logs_follow.go` — Log streaming
> - `internal/cliapp/bootstrap.go` — CLI bootstrap
> - `internal/cliapp/semver.go` — Semver helpers
> - `internal/lifecycle/lifecycle.go` (475 lines) — Run lifecycle policy
> - `internal/bootstrap/bootstrap.go` (285 lines) — Daemon bootstrap
> - `internal/version/version.go` (59 lines) — Version info
> - `internal/version/buildflags.go` (77 lines) — Build-time overrides

---

## 1. Version System

### 1.1 Version Variables (ldflags-overridable)

```go
var (
    Value           = "0.0.0-dev"
    VersionSource   = "internal/version/version.go"
    Channel         = "dev"          // "stable" for tagged releases
    APIVersion      = "v1"
    MinCliForDaemon = ""             // min CLI version the daemon accepts
    MinDaemonForCli = ""             // min daemon version the CLI expects
    GitCommitSHA    = ""
    BuildTimestamp  = ""
)
```

### 1.2 Build-time Override

Env vars for build-time injection:
- `LOOPER_BUILD_VERSION`, `LOOPER_BUILD_VERSION_SOURCE`, `LOOPER_BUILD_CHANNEL`
- `LOOPER_BUILD_API_VERSION`, `LOOPER_BUILD_MIN_CLI_FOR_DAEMON`, `LOOPER_BUILD_MIN_DAEMON_FOR_CLI`
- `LOOPER_BUILD_GIT_SHA`, `LOOPER_BUILD_TIMESTAMP`

### 1.3 Version Info (JSON)

```json
{
  "version": "1.2.3",
  "metadata": {
    "versionSource": "github-release",
    "channel": "stable",
    "apiVersion": "v1",
    "minCliForDaemon": null,
    "minDaemonForCli": null,
    "gitCommitSha": null,
    "buildTimestamp": null
  }
}
```

---

## 2. CLI Entry Point (`cmd/looper/main.go`)

```
looper [global flags] <command> [flags] [args]
```

Signal handling: SIGINT, SIGTERM → cancel context (via `signal.NotifyContext`).
`--version` flag: prints `version.Value` and exits 0.

Returns exit code:
- 0: success
- 1: generic error
- 2: "not ported yet" error

---

## 3. Complete Command Tree

### 3.1 Global Flags (apply to all commands)

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--json` | bool | false | Emit JSON output |
| `--no-auto-upgrade` | bool | false | Disable automatic upgrade checks |
| `--package-auto-upgrade-enabled` | string | "" | Enable auto-upgrade (bool) |
| `--config` | string | "~/.looper/config.toml" | Config path (.toml, .yaml, .json) |
| `--host` | string | "" | Server host |
| `--port` | string | "" | Server port |
| `--db-path` | string | "" | Database path |
| `--log-dir` | string | "" | Daemon log directory |
| `--daemon-mode` | string | "" | Daemon mode (foreground/launchd) |
| `--daemon-restart-policy` | string | "" | never/on-failure/always |
| `--daemon-restart-throttle-seconds` | string | "" | Restart throttle |
| `--git-path` | string | "" | Git binary path |
| `--gh-path` | string | "" | GitHub CLI path |
| `--looper-path` | string | "" | Looper CLI path |
| `--osascript-path` | string | "" | osascript path |
| `--planner-agent-timeout-seconds` | string | "" | Planner timeout |
| `--worker-agent-timeout-seconds` | string | "" | Worker timeout |
| `--reviewer-agent-timeout-seconds` | string | "" | Reviewer timeout |
| `--fixer-agent-timeout-seconds` | string | "" | Fixer timeout |
| `--roles-fixer-triggers-author-filter` | string | "" | current-user or any |
| `--roles-reviewer-behavior-loop-enabled-by-default` | string | "" | Loop enabled by default |
| `--roles-reviewer-discovery-triggers-enable-self-review` | string | "" | Self-review enabled |
| `--roles-reviewer-behavior-review-events-clean` | string | "" | COMMENT or APPROVE |
| `--roles-reviewer-behavior-review-events-blocking` | string | "" | COMMENT or REQUEST_CHANGES |
| `--roles-reviewer-behavior-loop-quiet-period-seconds` | string | "" | Quiet period |
| `--roles-reviewer-behavior-loop-min-publish-interval-seconds` | string | "" | Min publish interval |
| `--instructions-enabled` | string | "" | Enable custom instructions |

### 3.2 `looper status`

- **Path:** `status`
- **Args:** none
- **Flags:** (none beyond global)
- **API calls:**
  - `GET /api/v1/status` — full runtime status
- **Output:** human table or JSON `statusOutput` struct

### 3.3 `looper network`

- **Path:** `network`
- **Subcommands:**

#### `network join <url>`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--key` | string | "" | **Required.** One-time join key |
| `--name` | string | "" | **Required.** Node name (1-32 chars, `[A-Za-z0-9._-]`) |
| `--no-enroll-projects` | bool | false | Skip setting all projects to network.mode=routed |

- **Args:** 1 (URL of loopernet server)
- **API calls:**
  - `POST /v1/join` (to loopernet server) — cloud join
- **Output:** NetworkID, NodeID, NodeName, GitHub identity
- **Side effects:** Saves `~/.looper/network.json` with LocalState

#### `network leave`

- **Flags:** none
- **API calls:**
  - `POST /v1/leave` (to loopernet server)
- **Output:** left=true, restartRequired=true
- **Side effects:** Removes `network.json`, sets all projects to mode=off

#### `network status`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--verbose` | bool | false | Show extended membership details |

- **API calls:** `GET /v1/status` (to loopernet server)
- **Output:** `networkStatusOutput` struct

#### `network members`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--verbose` | bool | false | Show extended membership details |

- **API calls:** `GET /v1/status` (to loopernet server)
- **Output:** `networkMembersOutput` struct

### 3.4 `looper webhook`

- **Subcommands:** `enable`, `disable`, `status`, `cleanup <repo>`, `delete <repo>`, `rotate <repo>`, `list-orphans`

### 3.5 `looper netadmin`

- **Path:** `netadmin`

#### `netadmin onboard-repo <owner/repo>`
- **Flags:** none
- **API calls:**
  - Fetches webhook secret from loopernet via client
  - Calls `gh` CLI to initialize labels and ensure webhook
- **Output:** "Onboarded owner/repo to <url> (hookId=N)"

#### `netadmin offboard-repo <owner/repo>`
- **API calls:** Lists/deletes webhook hooks via `gh`
- **Output:** "Offboarded owner/repo from <url> (N hook(s) removed)"

#### `netadmin repo-status <owner/repo>`
- **Output:** "owner/repo webhook hooks targeting <url>: N"

### 3.6 `looper bootstrap`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--yes` | bool | false | Run non-interactively with defaults |
| `--force` | bool | false | Reinstall managed daemon |
| `--agent-vendor` | string | "" | Agent vendor for generated config |
| `--project-path` | string | "" | Add a default project |
| `--enable-local-token` | bool | false | Enable local token auth |
| `--disable-osascript` | bool | false | Disable osascript notifications |

### 3.7 `looper version`

- **Flags:** (none beyond global)
- **API calls:** `GET /api/v1/version` (best-effort daemon version), falls back to `GET /api/v1/status`
- **Output:** "CLI version: X\nlooperd server version: Y"

### 3.8 `looper project`

- **Persistent flags:** `--repo-path`, `--id`, `--name`, `--base-branch`, `--worktree-root`, `--repo`, `--snapshot-mode`
- **Subcommands:**

#### `project list`
- **API:** `GET /api/v1/projects`

#### `project add [path]`
- **API:** `POST /api/v1/projects`

#### `project remove [id]`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--force` | bool | false | Skip confirmation |

- **API:** `DELETE /api/v1/projects/{id}`

### 3.9 `looper config`

- **Subcommands:**

#### `config get <key>`
- **Args:** 1 (key like "roles.reviewer.behavior.reviewEvents.clean")

#### `config set <key> <value>`
- **Args:** 2 (key, value)

#### `config unset <key>`
- **Args:** 1 (key)

#### `config validate` / `config lint`
- **Flags:** none

#### `config show`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--source` | bool | false | Show with source layer |

- **API:** `GET /api/v1/config`

#### `config edit`
- Opens config in $EDITOR

#### `config migrate`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--from` | string | "~/.looper/config.json" | Source path |
| `--to` | string | Canonical TOML path | Destination |
| `--dry-run` | bool | false | Preview only |
| `--force` | bool | false | Overwrite with backup |

### 3.10 `looper prompt preview`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--project` | string | "" | Project ID |
| `--role` | string | "" | Role (planner/worker/reviewer/fixer) |

### 3.11 `looper daemon`

- **Persistent flags:** `--lines`, `--full`, `--startup`, `--force`

#### `daemon install`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--force` | bool | false | Overwrite existing |

- **Action:** Downloads looperd binary from GitHub releases (`nexu-io/looper`)
- **Release URL:** `https://api.github.com/repos/nexu-io/looper/releases/latest`
- **Supported targets:** darwin-arm64, linux-amd64
- **Install path:** `~/.looper/bin/looperd`
- **Archive format:** `.tar.gz` with SHA256 checksum

#### `daemon status`
- **API calls:** `GET /api/v1/status`, `GET /api/v1/healthz`
- **Output:** `daemonStatusOutput` struct

#### `daemon start`
- **Flags:** (none beyond daemon persistent)
- **Actions:**
  1. Read lifecycle state from `~/.looper/looperd.state.json`
  2. Check PID file at `~/.looper/looperd.pid`
  3. Verify no existing looperd is running
  4. If `daemon.mode=launchd` (macOS): write plist to `~/Library/LaunchAgents/io.nexu.looper.looperd.plist`, run `launchctl bootstrap`
  5. If `daemon.mode=foreground`: spawn detached process, write PID file
  6. Wait for API readiness (poll `/api/v1/status` up to 30s at 100ms intervals)

#### `daemon stop`
- **Actions:**
  1. Send SIGTERM to PID
  2. Wait for process exit
  3. If launchd mode: `launchctl bootout`

#### `daemon restart`
- Combines stop + start

#### `daemon logs`
- **Flags:** `--lines <count>`, `--full`, `--startup`
- **Reads:** `~/.looper/logs/looperd.log` (and rotation files)

### 3.12 `looper upgrade`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--check` | bool | false | Check available updates only |
| `--cli` | bool | false | Upgrade CLI binary |
| `--daemon` | bool | false | Install/upgrade daemon binary |
| `--background-auto` | bool (hidden) | false | Run auto-upgrade worker in background |

- **Auto-upgrade system:**
  - State file: `~/.looper/auto-upgrade.json`
  - Lock file: `~/.looper/auto-upgrade.lock`
  - Check interval: 24h
  - In-flight timeout: 30 min
  - Busy retry delay: 5 min
  - Only runs for "stable" channel release binaries (not dev builds)
  - Controlled by `package.autoUpgradeEnabled` config

### 3.13 `looper labels init`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--repo` | string | "" | GitHub repo slug |
| `--dry-run` | bool | false | Preview only |

### 3.14 `looper queue`

- **Subcommands:** `stats`, `list`, `failed`, `cleanup`

#### `queue list`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--eligible` | bool | false | Only eligible items |

#### `queue failed`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--type` | string | "" | Filter by type |
| `--project` | string | "" | Filter by project |
| `--limit` | string | "" | Max items |

#### `queue cleanup`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--stale` | bool | false | Cancel stale queued items |

### 3.15 `looper worktree cleanup`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--dry-run` | bool | false | Inspect only |
| `--confirm` | bool | false | Actually delete |

### 3.16 `looper loop`

- **Subcommands:** `list`, `inspect <id>`, `failures`, `start`, `pause [id]`, `retry <id>`

#### `loop list`
- **API:** `GET /api/v1/loops`

#### `loop inspect <seq|loopId|runId>`
- **API:** `GET /api/v1/loops/{id}`

#### `loop failures`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--type` | string | "" | Filter by loop type |
| `--project` | string | "" | Filter by project |
| `--limit` | string | "" | Max items |

#### `loop start`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--type` | string | "" | **Required.** Loop type |
| `--pr` | string | "" | **Required.** PR ref (owner/repo#N) |
| `--project` | string | "" | Project ID |

- **API:** `POST /api/v1/loops`

#### `loop pause [id]`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--id` | string | "" | Loop ID |

- **API:** `POST /api/v1/loops/{id}/pause`

#### `loop retry <seq|loopId>`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--mode` | string | "auto" | auto/resume/rediscover |
| `--discard-worktree-changes` | bool | false | Discard dirty changes (requires --confirm) |
| `--confirm` | bool | false | Confirm destructive action |

- **API:** `POST /api/v1/loops/{id}/retry`

### 3.17 `looper work`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--project` | string | "" | Project ID |
| `--title` | string | "" | Task title |
| `--prompt` | string | "" | Implementation prompt |
| `--issue` | string | "" | Issue number |
| `--spec` | string | "" | Spec file path |
| `--repo` | string | "" | Repo slug |
| `--base-branch` | string | "" | Base branch |

### 3.18 `looper plan`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--project` | string | "" | Project ID |
| `--issue` | string | "" | Issue number |

### 3.19 `looper pr`

- **Subcommands:** `list`, `show <ref>`, `status <ref>`

#### `pr list` — `GET /api/v1/pull-requests`
#### `pr show <ref>` — `GET /api/v1/pull-requests/{id}`
#### `pr status <ref>` — `GET /api/v1/pull-requests/{id}/status`

### 3.20 `looper review <pr>`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--project` | string | "" | Project ID |
| `--loop` | bool | false | Keep reviewing |
| `--no-loop` | bool | false | One pass only |
| `--clean-review-event` | string | "" | COMMENT or APPROVE |
| `--blocking-review-event` | string | "" | COMMENT or REQUEST_CHANGES |

- **API:** `POST /api/v1/loops`
- **Subcommands:** `review submit <pr>`, `review repair <pr>`

### 3.21 `looper fix <pr>`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--project` | string | "" | Project ID |
| `--loop` | bool | false | Keep fixing |
| `--no-loop` | bool | false | One pass only |

- **API:** `POST /api/v1/loops`

### 3.22 `looper takeover [<pr>]`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--agent-vendor` | string | "" | Agent vendor |
| `--merge` | bool | false | Enable auto-merge |
| `--no-fix` | bool | false | Reviewer only |
| `--yes` | bool | false | Non-interactive |

- **Subcommands:** `list`, `stop [<pr>]`

### 3.23 `looper feedback [message...]`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--title` | string | "" | Issue title hint |

### 3.24 `looper ps`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--all` | bool | false | Show all statuses |
| `--status` | string | "" | Filter by status |
| `--type` | string | "" | Filter by loop type |
| `--project` | string | "" | Filter by project |

### 3.25 `looper jump [id]`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--print-path` | bool | false | Print path only |
| `--shell-integration` | string | "" | bash/zsh/fish |

- **API:** `GET /api/v1/runs/active/{id}`

### 3.26 `looper logs <id>`

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--stderr` | bool | false | Show stderr |
| `--tail` | string | "" | Last N lines |
| `--full` | bool | false | Full output |
| `--follow` | bool | false | Stream (human only) |

### 3.27 `looper pause <seq>` / `looper unpause <seq>`

- **API:** `POST /api/v1/loops/{seq}/pause`, `POST /api/v1/loops/{seq}/start`
- **Args:** 1 (numeric sequence number)

### 3.28 `looper stop <id|all>`

- **Args:** 1 (loop ID or "all")
- **API:** `POST /api/v1/loops/{id}/stop`

### 3.29 `looper close <id>`

- **Args:** 1 (loop ID)
- **API:** `POST /api/v1/loops/{id}/close`

### 3.30 `looper run`

- **Persistent flags:** `--loop`
- **Subcommands:**

#### `run list` — shows runs
#### `run stats`
| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--since` | string | "24h" | Time window |
| `--role` | string | "" | Filter by role |

#### `run reconcile-stale` — triggers stale run reconciliation

### 3.31 `looper retry <seq|loopId>` (top-level alias)

Same as `loop retry`.

---

## 4. Daemon API Client

The CLI communicates with the daemon via a REST API client:

```go
type DaemonAPIClient struct {
    baseURL    string
    token      string
    httpClient *http.Client
}
```

**Methods:**
- `Get(ctx, path, out)` — GET request
- `Post(ctx, path, body, out)` — POST request
- `Delete(ctx, path, out)` — DELETE request
- `Stream(ctx, path, accept)` — streaming GET (returns *http.Response)

**Response format (JSON envelope):**
```json
{
  "requestId": "req_...",
  "ok": true,
  "data": { ... },
  "error": null
}
```

Error responses wrap `DaemonAPIError` with:
- `Message` — human-readable error
- `Code` — machine-readable error code (`api.ErrorCode`)
- `Status` — HTTP status code
- `RequestID` — correlation ID

---

## 5. Output Format

### 5.1 JSON output (all commands)

Every command supports `--json` flag (global). When set, the response payload is printed as formatted JSON to stdout.

### 5.2 Human output

Custom `renderHelp` function provides structured help text with:
- Usage line
- Version info (root only)
- Subcommand table (padded)
- Local flags
- Inherited (global) flags
- Examples

Status/output uses `printSection()` with `[][2]any` key-value rows and optional tables via `tableRow` map.

---

## 6. Daemon Entry Point (`cmd/looperd/main.go`)

### 6.1 Bootstrap Sequence

```
main()
  └─ run(args, stdout, stderr)
       ├─ Check --version → print version.Value, exit 0
       ├─ Check --help or "help" → print usage, exit 0
       └─ bootstrap.Bootstrap(ctx, Options{Args, Stdout, Stderr, WaitForShutdown: true})
            └─ Bootstrap()
                 ├─ 1. config.LoadFile() — load from CLI args + env + defaults
                 ├─ 2. Validate configured tool paths (git, gh, osascript)
                 ├─ 3. Ensure runtime directories are writable:
                 │      LogDir, DB parent dir, WorkingDirectory
                 ├─ 4. CreateLogger() — file-based (rotating) + stdout/stderr
                 ├─ 5. StartRuntime() → startRuntimeWithAPI():
                 │      ├─ runtime.New(Config, Logger, DeferRecovery: true)
                 │      ├─ rt.Start(ctx) — initialize scheduler, storage, etc.
                 │      ├─ api.NewHandler(...) — wire up API handler
                 │      ├─ api.NewServer(Config, Handler).Start()
                 │      ├─ rt.CompleteStartup(ctx) — finalize startup
                 │      └─ return daemonRuntime{runtime, server}
                 └─ 6. WaitForShutdown with signal notifier
```

### 6.2 Signal Handling

```
waitForShutdownWithSignals(runtime, logger, notifier):
  ├─ signal.Notify(signals, os.Interrupt, syscall.SIGTERM)
  ├─ Spawn goroutine:
  │     select {
  │       case sig := <-signals:
  │           runtime.Stop(signalReason(sig))
  │       case <-listenerStopped:
  │     }
  └─ runtime.WaitForShutdown()
       └─ daemonRuntime.Stop()
            └─ server.Stop(ctx) with shutdown timeout (default 1s)
            └─ runtime.Stop(reason)
```

### 6.3 Shutdown Sequence

```
daemonRuntime.Stop(reason):
  ├─ (once) server.Stop(ctx, timeout) — graceful HTTP drain
  └─ (once) runtime.Stop(reason) — stop scheduler, workers, webhook forwarders
```

### 6.4 Recovery on Restart

On restart, the daemon:

1. **Re-reads config** from scratch (no saved runtime state)
2. **Reconciles stale running runs** via `rt.ReconcileStaleRunningRuns(ctx)`:
   - Scans for runs with status "running" whose agent process has exited
   - Marks them as failed/terminated
3. **Re-checks coordinator lease** via `client/manager.go`:
   - Loads `network.json` state (if present)
   - Starts heartbeat loop (10s interval)
   - Reconciles coordinator lease (acquire/renew/expire based on eligibility)
4. **Re-registers webhook forwarders** from config
5. **Re-initializes scheduler** from persistent queue (SQLite)

### 6.5 Process Lifecycle State

Persisted at `~/.looper/looperd.state.json`:

```json
{
  "schemaVersion": 1,
  "mode": "foreground",
  "pid": 12345,
  "startedAt": "...",
  "binaryPath": "/path/to/looperd",
  "supervisor": {
    "source": "launchd",
    "label": "io.nexu.looper.looperd",
    "plistPath": "...",
    "restartPolicy": "on-failure",
    "restartThrottleSeconds": 5
  },
  "logs": {
    "main": "~/.looper/logs/looperd.log",
    "startupDir": "~/.looper/logs/startup",
    "stdout": "~/.looper/logs/launchd/looperd.stdout.log",
    "stderr": "~/.looper/logs/launchd/looperd.stderr.log"
  },
  "lastExit": {
    "at": "2026-06-21T12:00:00Z",
    "exitCode": 0,
    "reason": "SIGTERM"
  }
}
```

PID file at `~/.looper/looperd.pid` (simple text file with PID number).

### 6.6 Startup Logging

- Startup logs written to `~/.looper/logs/startup/` directory
- Main daemon log at `~/.looper/logs/looperd.log`
- launchd stdout/stderr at `~/.looper/logs/launchd/looperd.{stdout,stderr}.log`

### 6.7 Validated Tool Paths on Boot

Tools validated during bootstrap:
| Tool | Config field | Detection method |
|------|-------------|-----------------|
| `git` | `tools.gitPath` | `exec.LookPath` or configured path |
| `gh` | `tools.ghPath` | `exec.LookPath` or configured path |
| `osascript` | `tools.osascriptPath` | `exec.LookPath` or configured path |

If a path is explicitly configured (not auto-detected), the bootstrap verifies it points to an existing, executable file (not a directory). Errors are fatal (fail-fast).

### 6.8 Stale Run Reconciliation (stopAll)

The daemon exposes `POST /api/v1/loops/{id}/stop`, `POST /api/v1/loops/{id}/close`, and stop-all logic:

1. Collect candidates: loops in non-terminal status + runs with status "running"
2. For each candidate:
   - Pause loop (mark paused)
   - Find latest run and latest agent execution
   - If execution has active PID → send SIGTERM to process group
   - If execution is tracked via `ActiveExecutions` → kill via that mechanism
3. If `close` (terminal): also call `Loops.Terminate()`

Returns summary of stopped/already-finished/already-stopping/failed counts.

---

## 7. Auto-Upgrade System

### 7.1 State Schema

```json
{
  "schemaVersion": 1,
  "lastCheckedAt": "2026-06-21T00:00:00Z",
  "retryAfter": null,
  "inFlight": { "pid": 12345, "startedAt": "..." },
  "ready": {
    "cliChanged": true,
    "cliVersion": "1.2.4",
    "daemonChanged": false,
    "completedAt": "2026-06-21T00:05:00Z"
  }
}
```

### 7.2 Upgrade Flow

1. **Pre-command hook** (`maybeRunAutoUpgrade`):
   - Checks if CLI is a release binary (not dev build)
   - Checks if auto-upgrade is enabled in config
   - Acquires file lock (`~/.looper/auto-upgrade.lock`)
   - Reconciles state: if ready state exists → apply swap, notify
   - If interval elapsed (24h) → trigger check

2. **Check phase**: Query GitHub releases API for latest versions
3. **Download phase**: Download CLI/daemon binary with SHA256 verification
4. **Swap phase**: Atomic rename (`.new` → binary), restart daemon if upgraded

### 7.3 CLI Install Source Detection

Sources (ordered by priority):
- `release-binary`: binary from GitHub release (self-upgradable)
- `homebrew`: managed by Homebrew
- `dev`: built locally (version contains "dev")
- `unknown`: cannot determine

Only `release-binary` channel with `channel == "stable"` runs auto-upgrade.

---

## 8. Lifecycle Policy (`internal/lifecycle/lifecycle.go`)

### 8.1 Policy Types

```go
const PolicyAgentManagedWithFallback = "agent_managed_with_fallback"
const PolicyVersion = 1
```

### 8.2 Run Lifecycle State

```json
{
  "policy": "agent_managed_with_fallback",
  "policy_version": 1,
  "branch": "feat-123",
  "base_branch": "main",
  "planned_branch": "feat-123",
  "planned_base_branch": "main",
  "agent_branch": "feat-123-agent",
  "agent_base_branch": "main",
  "active_branch": "feat-123",
  "active_base_branch": "main",
  "branch_provenance": "planned",
  "commit_shas": ["abc123"],
  "pushed": true,
  "pr_number": 42,
  "pr_url": "https://github.com/owner/repo/pull/42",
  "pr_adopted": true,
  "actions": {
    "commit": "agent",
    "push": "agent",
    "pr": "agent"
  },
  "reconciled_at": "...",
  "reconciled_by": "...",
  "last_reconciliation_error": "",
  "agent_ingested_at": "..."
}
```

### 8.3 Action Sources

```go
ActionSourceNone     = "none"     // action not yet performed
ActionSourceAgent    = "agent"    // agent performed the action
ActionSourceFallback = "fallback" // looper performed the action as fallback
```

### 8.4 Branch Provenance

```go
BranchProvenancePlanned       = "planned"        // branch comes from plan
BranchProvenanceAgentMigrated = "agent_migrated" // agent changed the branch
```

### 8.5 Prompt Injection

The lifecycle policy is injected into agent prompts via `PromptInstruction()`:

```
Agent-managed git/PR lifecycle policy: agent_managed_with_fallback.
Before finishing: inspect git status, staged and unstaged diffs, untracked files,
and recent commit style; commit only relevant non-secret changes and push the
current branch; create or adopt an open pull request for this branch.
...
Expected lifecycle branch="feat-123" baseBranch="main" expectPush=true expectPR=true fallbackAllowed=true.
```

Includes disclosure stamping instructions based on config:
- Git commit trailers: `looper-generated-by: runner role`
- PR body footers: Markdown disclosure block
- Issue/comment footers: HTML + Markdown
- Inline review comments: hidden HTML marker

---

## 9. Daemon Install / Binary Distribution

### 9.1 Release Asset Discovery

Release assets named per target:
| Target | Asset pattern |
|--------|--------------|
| darwin-arm64 | `looperd-darwin-arm64.tar.gz` + `.sha256` |
| linux-amd64 | `looperd-linux-amd64.tar.gz` + `.sha256` |

Archives preferred when available (smaller), fallback to raw binary for older releases.

### 9.2 Download Verification

1. Download archive/binary
2. Download `.sha256` checksum file
3. Parse SHA-256 hex string
4. Verify `sha256(payload) == expected`
5. If archive: extract named binary from `.tar.gz` (reject path traversal via `/` prefix and `..`)

### 9.3 Install Paths

- Managed binary: `~/.looper/bin/looperd`
- CLI: current executable path (self-upgrade renames existing binary to `.old`)

---

## 10. Key Output Types Summary

| Structure | Used By |
|-----------|---------|
| `statusOutput` | `looper status` |
| `networkStatusOutput` | `looper network status` |
| `networkMembersOutput` | `looper network members` |
| `daemonStatusOutput` | `looper daemon status` |
| `daemonLogsOutput` | `looper daemon logs` |
| `daemonInstallResult` | `looper daemon install` |
| `upgradeCheckSummary` | `looper upgrade --check` |
| `cliUpgradeOutput` | `looper upgrade --cli` |
| `daemonUpgradeOutput` | `looper upgrade --daemon` |
| `unifiedUpgradeOutput` | `looper upgrade` (both) |
| `versionOutput` | `looper version` |
| `feedbackOutput` | `looper feedback` |
| `takeoverListOutput` | `looper takeover list` |
| `stopAllResponse` | `looper stop all` |

---

## 11. API Endpoints Called by CLI

| Method | Path | CLI Commands |
|--------|------|-------------|
| GET | `/api/v1/status` | `status`, `daemon status`, `version` |
| GET | `/api/v1/healthz` | `daemon status` |
| GET | `/api/v1/version` | `version` |
| GET | `/api/v1/config` | `config show` |
| GET | `/api/v1/projects` | `project list` |
| POST | `/api/v1/projects` | `project add` |
| DELETE | `/api/v1/projects/{id}` | `project remove` |
| GET | `/api/v1/loops` | `loop list` |
| POST | `/api/v1/loops` | `loop start`, `review`, `fix`, `work`, `plan` |
| GET | `/api/v1/loops/{id}` | `loop inspect` |
| POST | `/api/v1/loops/{id}/pause` | `loop pause`, `pause` |
| POST | `/api/v1/loops/{id}/start` | `unpause` |
| POST | `/api/v1/loops/{id}/retry` | `loop retry`, `retry` |
| POST | `/api/v1/loops/{id}/stop` | `stop` |
| POST | `/api/v1/loops/{id}/close` | `close` |
| GET | `/api/v1/pull-requests` | `pr list` |
| GET | `/api/v1/pull-requests/{id}` | `pr show` |
| GET | `/api/v1/pull-requests/{id}/status` | `pr status` |
| GET | `/api/v1/runs/active/{id}` | `jump` |
| POST | `/api/v1/reviewer/repair` | `review repair` |
| GET | `/api/v1/runs` | `run list` |
| POST | `/api/v1/runs/reconcile-stale` | `run reconcile-stale` |
| POST | `/api/v1/runs/stats` | `run stats` |
| GET | `/api/v1/events` | event log |
| POST | `/v1/join` | `network join` (to loopernet) |
| POST | `/v1/leave` | `network leave` (to loopernet) |
| GET | `/v1/status` | `network status`, `network members` (to loopernet) |
