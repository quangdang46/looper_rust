# Module 5: looper-agent — Executor (Rust Spec)

Source: `~/Projects/looper/internal/agent/executor.go` (1737 lines)
Tests: `~/Projects/looper/internal/agent/executor_test.go` (1253 lines)

---

## 1. Agent Vendors

Defined in `~/Projects/looper/internal/config/types.go`:

```go
type AgentVendor string

const (
    AgentVendorClaudeCode AgentVendor = "claude-code"
    AgentVendorCodex      AgentVendor = "codex"
    AgentVendorOpenCode   AgentVendor = "opencode"
    AgentVendorCursorCLI  AgentVendor = "cursor-cli"
    AgentVendorHermes     AgentVendor = "hermes"
)
```

### Native Resume Support
```go
func nativeResumeSupported(vendor config.AgentVendor) bool {
    // Supported: claude-code, codex, opencode, cursor-cli
    // NOT supported: hermes
}
```

---

## 2. Key Types

### ExecutorConfig
```go
type ExecutorConfig struct {
    Vendor              config.AgentVendor
    Model               *string
    Params              map[string]any   // includes key "args" ([]string) and "command" (string override)
    Env                 map[string]string
    NativeResumeEnabled bool
}
```

### RunInput
```go
type RunInput struct {
    ExecutionID        string
    ProjectID          string
    LoopID             string
    RunID              string
    Prompt             string
    NativeResumePrompt string
    WorkingDirectory   string
    Timeout            time.Duration
    HeartbeatTimeout   time.Duration
    GracefulShutdown   time.Duration
    MaxOutputBytes     int           // default: 256KB
    Metadata           map[string]any
    IdempotencyKey     string
    Env                map[string]string
    NativeSessionID    string
}
```

### Result
```go
type Result struct {
    Status                       string        // "completed" | "failed" | "timeout" | "killed"
    Summary                      string
    Stdout                       string
    Stderr                       string
    ParseStatus                  string        // "parsed" | "missing" | "invalid_json"
    CompletionSignal             string
    Artifacts                    []string
    ChangedFiles                 []string
    Commits                      []string
    Lifecycle                    *lifecycle.State
    HeartbeatCount               int64
    TimeoutType                  string        // "max_runtime" | "idle"
    ConfiguredIdleTimeoutSeconds int64
    ConfiguredMaxRuntimeSeconds  int64
    ElapsedRuntimeSeconds        int64
    LastProgressAt               string
    PID                          int
}
```

---

## 3. Command Path Resolution (`resolveCommand`)

```go
func resolveCommand(cfg ExecutorConfig) string {
    // 1. Check for command override in cfg.Params["command"]
    // 2. Otherwise, resolve by vendor:
    switch cfg.Vendor {
    case config.AgentVendorClaudeCode:  return "claude"
    case config.AgentVendorCursorCLI:   return "agent"
    default:                            return string(cfg.Vendor)
    }
}
```

| Vendor | Default Binary | Override Key |
|--------|---------------|--------------|
| claude-code | `claude` | `params.command` |
| codex | `codex` | `params.command` |
| opencode | `opencode` | `params.command` |
| cursor-cli | `agent` | `params.command` |
| hermes | `hermes` | `params.command` |

The binary is resolved from `$PATH` at spawn time via `exec.LookPath` (Go's `exec.Command` resolves it).

---

## 4. Argument Construction — EXACT Flags Per Vendor

### 4.1 Claude Code (`claude`)

**Fresh spawn:**
```
claude --model <model> --print <prompt> --dangerously-skip-permissions
```

**Native resume:**
```
claude --model <model> --resume <sessionID> --print <prompt> --dangerously-skip-permissions
```

Rules:
- Model flag: `--model` (prepended before sub-commands if present)
- Prompt flag: `--print` (appended with prompt text)
- Always appends `--dangerously-skip-permissions` (unless already present)
- Resume flag: `--resume <sessionID>` (unless `--continue` or `--resume` already present)

### 4.2 Codex (`codex`)

**Fresh spawn:**
```
codex exec --model <model> <prompt>
```

**Native resume:**
```
codex exec --model <model> <...args> resume <sessionID> <prompt>
```

Rules:
- Forces `exec` as first sub-command (unless already present in args)
- If args end with `-` (stdin mode), prompt is NOT appended
- Resume: inserts `resume <sessionID>` after `exec --model <model> <other-flags>`

### 4.3 OpenCode (`opencode`)

**Fresh spawn:**
```
opencode run --model <model> --dir <workingDirectory> <prompt>
```

**Native resume:**
```
opencode run --model <model> --dir <workingDirectory> --session <sessionID> <prompt>
```

Rules:
- Forces `run` as first sub-command (unless already present in args)
- `--dir <cwd>` is always inserted after `run` unless already specified
- Resume: uses `--session <sessionID>` (unless `--session` or `--continue` already present)
- If `-p`/`--prompt`/`-f`/`--file` already present, prompt is NOT appended

### 4.4 Cursor CLI (`agent`)

**Fresh spawn:**
```
agent --model <model> --print <prompt>
```

**Native resume:**
```
agent --model <model> --resume <sessionID> --print <prompt>
```

Rules:
- Model flag: `--model`
- Prompt flag: `--print`
- Resume flag: `--resume <sessionID>` (unless `--continue` or `--resume` already present)

### 4.5 Hermes (`hermes`)

**Fresh spawn:**
```
hermes -m <model> -z <prompt>
```

**Native resume:** (unsupported, falls back to fresh spawn)
```
hermes -z <prompt>
```

Rules:
- Uses short flags only: `-m` for model, `-z` for prompt
- Model flag comes BEFORE prompt flag (`-m <model> -z <prompt>`)
- Model flag omitted if model is nil/empty/whitespace
- Hermes does NOT support native resume → always falls back to `ResolveSpawn`

---

## 5. Working Directory Handling

```go
cmd.Dir = input.WorkingDirectory
```
- Set directly on `os/exec.Cmd.Dir`
- OpenCode additionally passes `--dir <workingDirectory>` as a CLI argument
- PWD env var is overridden: `envMap["PWD"] = workingDirectory`

---

## 6. Timeout Handling

### Two-tier Timeout System

**Max Runtime Timer (`input.Timeout`):**
- Fires once after `input.Timeout` duration
- Sets `timeoutType = "max_runtime"`
- Sends SIGTERM to process group
- Wait for `gracefulShutdown` (default 5s), then SIGKILL

**Idle / Heartbeat Timer (`input.HeartbeatTimeout`):**
- Ticks every 1 second (capped at 1s minimum)
- Checks `timeSinceLastOutput() >= heartbeatTimeout`
- If no output for longer than heartbeatTimeout → fires
- Sets `timeoutType = "idle"`
- Same SIGTERM → SIGKILL escalation

```go
type timeoutType string
const maxRuntimeTimeout = "max_runtime"
const idleTimeout      = "idle"
```

### Process Group Signal Escalation
1. `SIGTERM` to process group (`syscall.Kill(-pid, SIGTERM)`)
2. Wait `gracefulShutdown` (default 5 seconds)
3. `SIGKILL` to process group (`syscall.Kill(-pid, SIGKILL)`)

All signal names: `SIGTERM`, `SIGKILL` (first SIGTERM, then SIGKILL after grace).

---

## 7. Stdout/Stderr Parsing

### Stream Capture
```go
type streamCapture struct {
    onChunk func([]byte)
}
```
- Output is captured chunk-by-chunk via Write() callback
- Each chunk triggers: heartbeat update + persisted log append + DB status update

### Completion Marker Parsing (`parseCompletion`)
The agent is expected to emit a final line:
```
__LOOPER_RESULT__={"summary":"...","artifacts":[...],"changedFiles":[...],"commits":[...],"git_pr_lifecycle":{...}}
```

Parsing algorithm:
1. Search stdout + stderr (joined) in **reverse line order**
2. Find last occurrence of `__LOOPER_RESULT__=`
3. Parse JSON payload after prefix
4. Extract: summary, artifacts, changedFiles, commits, git_pr_lifecycle
5. Template detection: skips if summary is literal `<one-sentence summary>` (placeholder)
6. Status: `"parsed"` if found and valid JSON, `"invalid_json"` if malformed, `"missing"` if absent

### Completion Instruction Injection
```go
func AppendCompletionInstruction(prompt string) string
```
Appends to prompt:
```
When finished, print exactly one final line to stdout in this format:
__LOOPER_RESULT__={"summary":"<one-sentence summary>"}
Do not wrap that line in markdown.
Do not print anything after that line.
```

### Native Session ID Extraction (`extractNativeSessionID`)
Scans stdout + stderr line-by-line for JSON keys:
```
nativeSessionId, native_session_id, sessionId, session_id, chatId, chat_id
```
- First tries JSON parsing of each line
- Falls back to key:value / key=value extraction with bound checks
- Used to capture session IDs emitted by vendors during execution

---

## 8. Native Resume Support (Per Vendor)

| Vendor | Native Resume | Resume Flag | Session Source |
|--------|--------------|-------------|----------------|
| claude-code | ✅ Yes | `--resume <sessionID>` | `NativeSessionID` from DB |
| codex | ✅ Yes | `exec ... resume <sessionID>` | `NativeSessionID` from DB |
| opencode | ✅ Yes | `--session <sessionID>` | `NativeSessionID` from DB |
| cursor-cli | ✅ Yes | `--resume <sessionID>` | `NativeSessionID` from DB |
| hermes | ❌ No | N/A (always falls back) | N/A |

### Native Resume Resolution Logic
1. If `NativeResumeEnabled=false` → checkpoint_restart
2. If `input.NativeSessionID` is set → use it directly (if vendor supported)
3. Otherwise, look up `latest agent execution by LoopID` from DB
4. Conditions for resuming:
   - Same vendor as current config
   - Vendor supports native resume
   - Latest execution has `NativeSessionID` set
   - Latest execution has `NativeResumeStatus == "pending"`
   - Latest execution status is one of: `running`, `cancelling`, `killed`, `timeout`, `failed`, `completed`

### Fallback on Resume Failure
1. If native resume command fails to start → log fallback
2. Re-spawn with fresh prompt (`ResolveSpawn` without native resume args)
3. Clear native session ID, set mode to "checkpoint_restart"

### Lifecycle Events (at runtime.go level)
```
agent.invoked                  → agent start
agent.completed                → normal completion
agent.idle_timeout             → idle timeout
agent.max_runtime_timeout      → max runtime timeout
agent.timed_out                → generic timeout
agent.killed                   → killed
agent.native_resume_fallback_started  → resumed, then fell back
```

---

## 9. Environment Variable Injection

### Building the Env (`buildCommandEnv`)

```go
func buildCommandEnv(workingDirectory string, prompt string, envSources ...map[string]string) []string
```

Algorithm:
1. Start with `os.Environ()` (current process env)
2. Apply config env sources in order (`cfg.Env`, `input.Env`)
3. **Delete "unsafe" Git env vars** (16 keys) to prevent Git directory confusion attacks
4. Set `PWD = workingDirectory`
5. Set `LOOPER_PROMPT = prompt`
6. Set `LOOPER_COMPLETION_MARKER = __LOOPER_RESULT__=`
7. Sort keys alphabetically, return as `KEY=VALUE` slice

### Unsafe Environment Keys (stripped)
```go
var unsafeAgentEnvKeys = []string{
    "OLDPWD",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_CONFIG", "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT", "GIT_OBJECT_DIRECTORY", "GIT_DIR", "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE", "GIT_GRAFT_FILE", "GIT_COMMON_DIR", "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS", "GIT_REPLACE_REF_BASE", "GIT_PREFIX", "GIT_SHALLOW_FILE",
}
```

### Injected Env Vars
| Variable | Value | Purpose |
|----------|-------|---------|
| `LOOPER_PROMPT` | Full prompt text | Available for agent scripts |
| `LOOPER_COMPLETION_MARKER` | `__LOOPER_RESULT__=` | Signals expected output format |
| `PWD` | WorkingDirectory | Overrides inherited PWD |

---

## 10. Model Flag Handling

### `prependModelFlag` Logic
```go
func prependModelFlag(args []string, model *string, flag string, recognizedFlags []string) []string
```
- If model is nil/empty → no-op
- If any recognized flag already present → no-op
- If args start with `exec` or `run` → inserts after first arg: `[exec, --model X, ...]`
- Otherwise → prepends: `[--model X, ...]`

### Per-Vendor Model Flag Detail

| Vendor | Flag | Position |
|--------|------|----------|
| Claude Code | `--model <name>` | Before `--print` |
| Codex | `--model <name>` | After `exec` subcommand |
| OpenCode | `--model <name>` | After `run` subcommand |
| Cursor CLI | `--model <name>` | Before `--print` |
| Hermes | `-m <name>` | Before `-z` prompt |

---

## 11. Agent Execution Record (Persisted State)

Defined in `~/Projects/looper/internal/storage/repositories.go`:

```go
type AgentExecutionRecord struct {
    ID                 string
    ProjectID          *string
    LoopID             *string
    RunID              *string
    Vendor             string      // e.g., "claude-code", "codex", "opencode", "cursor-cli", "hermes"
    Status             string      // "running", "cancelling", "completed", "failed", "interrupted"
    PID                *int64
    CommandJSON        *string     // JSON: {"command": "claude", "args": ["--model", "..."]}
    CWD                *string
    Summary            *string
    ParseStatus        *string     // "parsed", "missing", "invalid_json"
    CompletionSignal   *string     // "__LOOPER_RESULT__="
    HeartbeatCount     int64
    LastHeartbeatAt    *string
    OutputJSON         *string     // {"stdout":"...","stderr":"...","stdoutLogPath":"...","stderrLogPath":"..."}
    ErrorMessage       *string
    NativeSessionID    *string
    NativeResumeMode   *string     // "native_resume", "checkpoint_restart"
    NativeResumeStatus *string     // "started", "disabled", "unsupported", "unavailable", "pending", "captured", "failed", "fallback_started", "fallback_completed", "fallback_failed"
    NativeResumeError  *string
    StartedAt          string      // ISO format
    EndedAt            *string
    MetadataJSON       *string     // includes idempotencyKey, metadata, timeoutPolicy, timeout info
    CreatedAt          string
    UpdatedAt          string
}
```

---

## 12. Log Persistence

### Log File Layout
```
<logDir>/loops/<loopID>/<runID>/<executionID>.stdout.log
<logDir>/loops/<loopID>/<runID>/<executionID>.stderr.log
```

- Created at execution start (truncated)
- Appended on each output chunk
- On completion, read back from persisted logs if write didn't fail
- Max read size: 16MB (`maxPersistedLogReadBytes`)
- In-memory buffer capped at `defaultMaxOutputBytes` (256KB) per stream

---

## 13. Process Lifecycle

### Start (`ConfiguredExecutor.Start`)
1. Validate input (prompt + working directory required)
2. Resolve native resume
3. Resolve command + args (`ResolveSpawnWithNativeResume`)
4. Create `exec.Cmd` with `Setpgid: true` (creates process group)
5. Build environment
6. Create stdout/stderr pipes via `streamCapture`
7. On resume failure: fallback to fresh spawn
8. Start goroutine `x.run(ctx)` for lifecycle management

### Wait (`execution.Wait`)
- Blocks on `doneCh` until process completes
- Re-publishes result to subsequent Wait() callers

### Kill (`execution.Kill`)
- Sends reason string to `killCh` channel
- Actual termination handled by `run()` goroutine

### Run Loop (`execution.run`)
1. Start process and wait in goroutine
2. Multi-way select:
   - `waitCh` → process exited
   - `timeoutTimer` → max runtime exceeded
   - `inactivityTimer` → no output for heartbeatTimeout
   - `killCh` → external kill request
   - `ctx.Done()` → context cancelled
   - `graceKillTimer` → grace period expired, SIGKILL
3. On timeout/kill: SIGTERM → (after grace) SIGKILL
4. On process exit: classify final status, parse completion, persist

### Status Classification
```go
func (x *execution) finalStatus(timedOut, killed bool) string {
    if timedOut { return "timeout" }
    if killed   { return "killed" }
    if exit code == 0 { return "completed" }
    return "failed"
}
```

---

## 14. Scheduler Adapters (Runtime Integration)

File: `~/Projects/looper/internal/runtime/scheduler.go`

Four adapters bridge `agent.ConfiguredExecutor` to runner-specific interfaces:

| Adapter | Runner | Extra Fields |
|---------|--------|-------------|
| `plannerAgentExecutorAdapter` | planner | - |
| `reviewerAgentExecutorAdapter` | reviewer | `NativeResumePrompt` |
| `fixerAgentExecutorAdapter` | fixer | - |
| `workerAgentExecutorAdapter` | worker | `ActiveExecutionRegistry` |

All adapters call `a.executor.Start(ctx, agent.RunInput{...})` with runner-specific field mappings.

Worker adapter additionally:
- Registers execution in `ActiveExecutionRegistry`
- Unregisters when execution completes

### Agent Executor Creation
```go
agentExecutor := agent.New(agent.ExecutorOptions{
    Config: agent.ExecutorConfig{
        Vendor:              *cfg.Agent.Vendor,
        Model:               cfg.Agent.Model,
        Params:              cfg.Agent.Params,
        Env:                 cfg.Agent.Env,
        NativeResumeEnabled: cfg.Agent.NativeResume.Enabled,
    },
    Repos:  repos,
    LogDir: cfg.Daemon.LogDir,
    Now:    now,
})
```

---

## 15. Setup Failure Detection

```go
func IsAgentSetupFailureMessage(message string) bool
```
Detects:
- Codex version mismatch: `" model requires a newer version of codex"` + `"please upgrade to the latest app or cli"`
- Model setup failures: "unsupported model", "unknown model", "invalid model", "model is not supported", "unrecognized model" + presence of agent name (codex/claude/opencode/cursor/hermes) + model configuration context

---

## 16. Test-Verified Command Examples (from executor_test.go)

### Fresh Spawn (model="gpt-5", workdir="/tmp/looper-worktree", prompt="hello")

| Vendor | Command | Arguments |
|--------|---------|-----------|
| claude-code | `claude` | `--model gpt-5 --print hello --dangerously-skip-permissions` |
| codex | `codex` | `exec --model gpt-5 hello` |
| opencode | `opencode` | `run --model gpt-5 --dir /tmp/looper-worktree hello` |
| cursor-cli | `agent` | `--model gpt-5 --print hello` |
| hermes | `hermes` | `-m gpt-5 -z hello` |

### Native Resume (sessionID="session-123", prompt="hello")

| Vendor | Arguments |
|--------|-----------|
| claude-code | `--resume session-123 --print hello --dangerously-skip-permissions` |
| codex | `exec resume session-123 hello` |
| opencode | `run --dir /tmp/looper-worktree --session session-123 hello` |
| cursor-cli | `--resume session-123 --print hello` |
| hermes | `-z hello` (unsupported, no model) |

### Custom Args (Params: `{"args": ["--profile", "test", "exec"]}`)

| Vendor | Arguments |
|--------|-----------|
| codex with custom args | `--model gpt-5 --profile test exec hello` (exec not duplicated) |
| opencode with custom args | `--model gpt-5 --profile test run --dir /tmp/looper-worktree hello` (run not duplicated) |

---

## 17. Config Agent Timeouts

```go
type AgentTimeoutConfig struct {
    PlannerSeconds  int
    WorkerSeconds   int
    ReviewerSeconds int
    FixerSeconds    int
    PlannerIdleTimeoutSeconds  int
    PlannerMaxRuntimeSeconds   int
    WorkerIdleTimeoutSeconds   int
    WorkerMaxRuntimeSeconds    int
    ReviewerIdleTimeoutSeconds int
    ReviewerMaxRuntimeSeconds  int
    FixerIdleTimeoutSeconds    int
    FixerMaxRuntimeSeconds     int
}
```

Each runner (planner/worker/reviewer/fixer) can have independent idle and max-runtime timeouts.
