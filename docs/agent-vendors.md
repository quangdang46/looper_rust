# Agent Vendors

Looper spawns AI coding agent processes as subprocesses. The `looper-agent` crate handles the full lifecycle: command resolution, process spawning, two-tier timeout, native resume, completion parsing, and environment hardening.

## Supported Vendors

The `AgentCliVendor` enum (`crates/looper-agent/src/types.rs`) defines five built-in vendors:

| Variant     | Config key       | Default binary | Model flag     | Prompt flag   | Forced subcommand | Native resume |
|-------------|------------------|----------------|----------------|---------------|-------------------|---------------|
| `ClaudeCode` | `"claude-code"`  | `claude`       | `--model`      | `--print`     | none              | `--resume`    |
| `Codex`      | `"codex"`        | `codex`        | `--model`      | (positional)  | `exec`            | `resume <sid>` |
| `Opencode`   | `"opencode"`     | `opencode`     | `--model`      | (positional)  | `run`             | `--session`   |
| `CursorCli`  | `"cursor-cli"`   | `agent`        | `--model`      | `--print`     | none              | `--resume`    |
| `Hermes`     | `"hermes"`       | `hermes`       | `-m`           | `-z`          | none              | unsupported   |

### Vendor-specific behavior

**Claude Code** (`claude-code`)

Additional flags added automatically:
- `--dangerously-skip-permissions` to suppress the interactive permission prompt.
- `--model <name>` when a model is configured.
- `--print <prompt>` for the instruction text.

Native resume: `--resume <sessionID> --print <prompt>`.

**Codex** (`codex`)

The command is `codex exec --model <name> <prompt>`. The prompt is passed as the last positional argument. For native resume the subcommand becomes `codex exec resume <sessionID> <prompt>`.

Codex also supports stdin mode: when the params include an args list ending with `"-"`, the prompt flag is not appended and the agent reads from stdin.

**OpenCode** (`opencode`)

The command is `opencode run --model <name> --dir <cwd> <prompt>`. The working directory is passed via `--dir`. Native resume uses `--session <sessionID>`.

**Cursor CLI** (`cursor-cli`)

Default binary is `agent`. Arguments: `--model <name> --print <prompt>`. Resume: `--resume <sessionID> --print <prompt>`.

**Hermes** (`hermes`)

Uses `-m <name>` for model and `-z <prompt>` for the instruction prompt. Does not support native resume. When native resume is requested for Hermes, the executor falls back to the checkpoint restart strategy (fresh spawn each time).

## Configuration

Agent vendors are configured in `looper.toml` under the `[agent]` section and are consumed by the `ExecutorConfig` struct:

```rust
pub struct ExecutorConfig {
    pub vendor: AgentCliVendor,
    pub model: Option<String>,
    pub params: HashMap<String, serde_json::Value>,
    pub env: HashMap<String, String>,
    pub native_resume_enabled: bool,   // default: true
}
```

### Basic `looper.toml` example

```toml
[agent]
default-vendor = "claude"
timeout-secs = 300
max-retries = 5
```

This controls the model provider for LLM API calls (`AgentVendor` in `looper-types`), not the CLI vendor used to execute agent processes. The CLI vendor selection lives in the runner dispatch layer.

### How CLI vendors are selected

The `ConfiguredExecutor` is constructed with an `ExecutorConfig` that specifies the CLI vendor, model override, extra environment variables, and native resume behavior. The runner roles (Planner, Reviewer, Worker, Fixer) each hold their own `ConfiguredExecutor` instance.

The full `RunInput` passed to the executor includes:

| Field                  | Type              | Default          | Description                                 |
|------------------------|-------------------|------------------|---------------------------------------------|
| `execution_id`         | `String`          | `""`             | Unique execution identifier                 |
| `project_id`           | `String`          | `""`             | Owning project                              |
| `loop_id`              | `String`          | `""`             | Owning loop                                 |
| `run_id`               | `String`          | `""`             | Owning run                                  |
| `prompt`               | `String`          | `""`             | Instruction text sent to the agent          |
| `native_resume_prompt` | `Option<String>`  | `None`           | Alternative prompt for resumed sessions     |
| `working_directory`    | `String`          | `""`             | Agent working directory (worktree path)     |
| `timeout`              | `Duration`        | `1800s` (30 min) | Maximum wall-clock runtime                  |
| `heartbeat_timeout`    | `Duration`        | `120s` (2 min)   | Idle timeout since last output              |
| `graceful_shutdown`    | `Duration`        | `5s`             | SIGTERM-to-SIGKILL grace period             |
| `max_output_bytes`     | `usize`           | `256 KiB`        | Max captured stdout+stderr                  |
| `native_session_id`    | `Option<String>`  | `None`           | Existing session to resume                  |
| `env`                  | `HashMap<String, String>` | `{}`     | Extra environment variables                 |
| `metadata`             | `HashMap<String, Value>` | `{}`     | Arbitrary metadata                          |
| `idempotency_key`      | `String`          | `""`             | Idempotency key for restart safety          |

### Custom params overrides

The `params` field lets you override the binary and arguments for any vendor, effectively supporting user-defined commands. All params are optional.

| Param key  | Type           | Effect                                                  |
|------------|----------------|---------------------------------------------------------|
| `command`  | `String`       | Override the binary path/name entirely                  |
| `args`     | `Array<String>`| Override the full argument list (base only; model flag, prompt, and resume flags are still added on top) |

Example configuring a custom command with custom base arguments:

```toml
# This would be passed programmatically; not a direct TOML section currently.
# The equivalent ExecutorConfig constructor:
# ExecutorConfig {
#     vendor: AgentCliVendor::ClaudeCode,
#     model: Some("gpt-5".to_string()),
#     params: {
#         "command": json!("/opt/bin/my-custom-agent"),
#         "args": json!(["--profile", "custom", "exec"]),
#     },
#     ..
# }
```

When `command` is set in params, it replaces the default binary from `AgentCliVendor::default_binary()`. When `args` is set in params, it replaces the default empty argument list. Model flags, prompt flags, working directory flags, and resume flags are still appended on top.

## Process Lifecycle

### Spawning

The executor (`ConfiguredExecutor::start()`) resolves the spawn command, builds the environment, and forks the process.

```rust
let cmd = resolve_spawn(&config, &working_directory, &prompt);
let mut child = Command::new(&cmd.binary)
    .args(&cmd.args)
    .env_clear()
    .envs(env_vars)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .current_dir(&working_directory)
    .spawn()?;
```

Each agent process is placed in its own process group via `setpgid` in a `pre_exec` hook, allowing clean escalation sends SIGTERM and SIGKILL to the entire process tree.

### Two-tier timeout

The `run_loop()` method implements two independent timers:

1. **Max runtime** -- hard wall-clock limit (default 30 minutes). When exceeded, the process group receives SIGTERM.
2. **Idle/heartbeat timeout** -- resets on every byte of stdout output (default 2 minutes). When no output arrives within the window, the process group receives SIGTERM.

In both cases, after the grace period (`graceful_shutdown`, default 5 seconds), SIGKILL is sent to force termination.

### Completion marker

Agents must print a final line to stdout in this exact format:

```
__LOOPER_RESULT__={"summary":"<one-sentence summary>","artifacts":[],"changedFiles":[],"commits":[]}
```

The marker `__LOOPER_RESULT__=` is appended to every prompt via `append_completion_instruction()`. The `parse_completion()` function scans output in reverse line order for the last occurrence. The `CompletionPayload` structure supports:

| Field            | Type                   | Description                        |
|------------------|------------------------|------------------------------------|
| `summary`        | `String`               | One-sentence task summary          |
| `artifacts`      | `Vec<String>`          | Paths to files created/modified    |
| `changed_files`  | `Vec<String>`          | Paths of changed files             |
| `commits`        | `Vec<String>`          | Commit SHAs                        |
| `git_pr_lifecycle` | `Option<Value>`      | PR lifecycle metadata (optional)   |

## Native Resume

Looper supports resuming agent sessions natively (via each vendor's own session mechanism) for all vendors except Hermes. The feature is enabled by default and controlled by `ExecutorConfig::native_resume_enabled` (default: `true`).

### Resume flow

1. When a loop execution completes or fails, the executor stores the native session ID extracted from the agent's output.
2. On the next execution for the same loop, `resolve_spawn_with_resume()` checks if a resumable session exists in the database.
3. A session is considered resumable when vendor matches, a native session ID is present, the resume status is `"pending"`, and the execution status is one of `running`, `cancelling`, `killed`, `timeout`, `failed`, or `completed`.
4. If a resumable session is found, the vendor-specific resume flags are used instead of constructing a fresh command.
5. If native resume is disabled or unsupported, the executor falls back to `CheckpointRestart` (fresh spawn with the original prompt).

### NativeResumeMode

| Variant              | Description                                       |
|----------------------|---------------------------------------------------|
| `NativeResume`       | Resume using the vendor's built-in session ID     |
| `CheckpointRestart`  | Fresh spawn without resume (fallback)             |

### NativeResumeStatus

| Variant              | Meaning                                       |
|----------------------|-----------------------------------------------|
| `Started`            | Native resume was initiated successfully      |
| `Disabled`           | Native resume is disabled in config           |
| `Unsupported`        | Vendor does not support native resume         |
| `Unavailable`        | No resumable session found                    |
| `Pending`            | Session captured but not yet resumed          |
| `Captured`           | Session ID captured from agent output         |
| `Failed`             | Resume attempted but failed                   |
| `FallbackStarted`    | Started with fallback strategy                |
| `FallbackCompleted`  | Fallback completed successfully               |
| `FallbackFailed`     | Fallback also failed                          |

## Environment Variables

### Variables injected into the agent process

The `build_command_env()` function (`crates/looper-agent/src/env.rs`) constructs the environment for agent processes:

| Variable                | Source                     | Description                            |
|-------------------------|----------------------------|----------------------------------------|
| `PWD`                   | Working directory          | Set to the agent's working directory   |
| `LOOPER_PROMPT`         | Run input prompt           | The full instruction text              |
| `LOOPER_COMPLETION_MARKER` | Hardcoded              | The `__LOOPER_RESULT__=` marker string |
| Inherited env vars      | Current process            | All current environment variables pass through, then config/env overrides apply |

### Config-level env overrides

Each `ExecutorConfig` carries an `env: HashMap<String, String>` that is applied on top of the inherited environment. These override any inherited values.

### Run-level env overrides

Each `RunInput` carries its own `env: HashMap<String, String>` with even higher precedence, applied after config env.

### Security: unsafe Git env stripping

The following environment variables are always stripped to prevent directory confusion attacks:

| Variable                            |
|-------------------------------------|
| `OLDPWD`                            |
| `GIT_ALTERNATE_OBJECT_DIRECTORIES`  |
| `GIT_CONFIG`                        |
| `GIT_CONFIG_PARAMETERS`             |
| `GIT_CONFIG_COUNT`                  |
| `GIT_OBJECT_DIRECTORY`              |
| `GIT_DIR`                           |
| `GIT_WORK_TREE`                     |
| `GIT_IMPLICIT_WORK_TREE`            |
| `GIT_GRAFT_FILE`                    |
| `GIT_COMMON_DIR`                    |
| `GIT_INDEX_FILE`                    |
| `GIT_NO_REPLACE_OBJECTS`            |
| `GIT_REPLACE_REF_BASE`              |
| `GIT_PREFIX`                        |
| `GIT_SHALLOW_FILE`                  |

The resulting environment list is sorted lexicographically for deterministic output.

## Setup Failure Detection

The `detect_agent_setup_failure()` function checks agent output for common configuration errors:

- Codex version mismatch messages ("model requires a newer version of codex" / "please upgrade")
- Unsupported or unknown model messages ("unsupported model", "unknown model", "invalid model", "model is not supported", "unrecognized model")
- Detection requires the message to mention both a model and one of the agent names (codex, claude, opencode, cursor, hermes)

## Adding a New Vendor

To add a new agent CLI vendor, you need to modify two files:

### 1. Add the variant to `AgentCliVendor` (`crates/looper-agent/src/types.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentCliVendor {
    // ... existing variants ...
    #[serde(rename = "my-vendor")]
    MyVendor,
}
```

Then implement all the methods:

```rust
impl AgentCliVendor {
    pub fn as_str(&self) -> &'static str {
        match self {
            // ...
            Self::MyVendor => "my-vendor",
        }
    }

    /// Whether this vendor supports native resume.
    pub fn native_resume_supported(&self) -> bool {
        match self {
            // ...
            Self::MyVendor => true, // or false
        }
    }

    /// Default binary name for this vendor.
    pub fn default_binary(&self) -> &'static str {
        match self {
            // ...
            Self::MyVendor => "my-agent-cli",
        }
    }

    /// Model flag for this vendor.
    pub fn model_flag(&self) -> &'static str {
        match self {
            // ...
            Self::MyVendor => "--model",
        }
    }

    /// Prompt flag for this vendor.
    pub fn prompt_flag(&self) -> &'static str {
        match self {
            // ...
            Self::MyVendor => "--print",
        }
    }

    /// Subcommand to force (e.g. "exec" for codex, "run" for opencode).
    pub fn forced_subcommand(&self) -> Option<&'static str> {
        match self {
            // ...
            Self::MyVendor => None,
        }
    }

    /// Resume flag.
    pub fn resume_flag(&self) -> &'static str {
        match self {
            // ...
            Self::MyVendor => "--resume",
        }
    }
}
```

### 2. Update argument construction (`crates/looper-agent/src/args.rs`)

The `resolve_spawn()` and `resolve_spawn_with_native_resume()` functions contain vendor-specific logic:

- **`resolve_spawn()`**: Handles fresh spawns. Add branches for your vendor's forced subcommand, working directory flags, prompt position, and any special flags.
- **`resolve_spawn_with_native_resume()`**: Handles resumed sessions. Add the appropriate resume argument syntax for your vendor.

Example for a vendor that takes `--resume <sessionID> --prompt <text>`:

```rust
// In resolve_spawn_with_native_resume():
AgentCliVendor::MyVendor => {
    args.push("--resume".to_string());
    args.push(session_id.to_string());
    args.push("--prompt".to_string());
    args.push(prompt.to_string());
}
```

### 3. Update setup failure detection (`crates/looper-agent/src/env.rs`)

Add the vendor name to the agent name list in `detect_agent_setup_failure()`:

```rust
let has_agent_name = ["codex", "claude", "opencode", "cursor", "hermes", "my-vendor"]
    .iter()
    .any(|n| lower.contains(n));
```

### 4. (Optional) Update `AgentVendor` in `looper-types`

If the new CLI vendor also maps to a model provider for API calls, add it to the `AgentVendor` enum in `crates/looper-types/src/lib.rs`.

### 5. Add tests

Add test functions in `args.rs` for both fresh spawn and native resume variants, following the existing patterns:

```rust
#[test]
fn test_my_vendor_fresh_spawn() {
    let cfg = make_config(AgentCliVendor::MyVendor, Some("gpt-5"));
    let cmd = resolve_spawn(&cfg, "/tmp/worktree", "hello");
    assert_eq!(cmd.binary, "my-agent-cli");
    // Assert expected flags...
}

#[test]
fn test_my_vendor_native_resume() {
    let cfg = make_config(AgentCliVendor::MyVendor, Some("gpt-5"));
    let cmd = resolve_spawn_with_native_resume(&cfg, "/tmp/worktree", "session-123", "hello");
    // Assert expected resume flags...
}
```

### Shortcut: custom params instead of code changes

If you only need to run a different binary without adding first-class support, use the `params.command` and `params.args` overrides in `ExecutorConfig`. This works with any existing vendor variant and does not require any Rust code changes:

```rust
ExecutorConfig {
    vendor: AgentCliVendor::ClaudeCode, // base vendor for flag patterns
    params: {
        "command": json!("/path/to/custom-binary"),
        "args": json!(["-custom-flag"]),
    },
    // Model and prompt flags still use the base vendor's patterns
    ..
}
```

The base vendor's flag resolution (`model_flag`, `prompt_flag`, `resume_flag`, etc.) still applies on top of the custom args, so choose the base vendor whose flag patterns most closely match your custom binary.

## Migration from Hermes

If you were using Hermes in a legacy setup and need to migrate to a supported vendor:

- Hermes does not support native resume; all other vendors do.
- Hermes uses `-m` for model and `-z` for prompt; other vendors use `--model` and `--print` (or positional).
- Recommended replacement: use `CursorCli` (cursor-cli) or `ClaudeCode` (claude-code) depending on your use case.

## Logging and Debugging

Each agent execution writes separate log files:

```
<log-dir>/loops/<loop-id>/<run-id>/<execution-id>.stdout.log
<log-dir>/loops/<loop-id>/<run-id>/<execution-id>.stderr.log
```

The `agent_executions` database table records the full command JSON, environment, timing, heartbeat count, native session ID, and completion status for every execution.

## Source Map

| File                                                  | Purpose                                   |
|-------------------------------------------------------|-------------------------------------------|
| `crates/looper-agent/src/types.rs`                    | `AgentCliVendor` enum, `ExecutorConfig`, `RunInput`, `AgentResult`, state types |
| `crates/looper-agent/src/args.rs`                     | `resolve_spawn()`, `resolve_spawn_with_native_resume()`, `append_completion_instruction()` |
| `crates/looper-agent/src/env.rs`                      | `build_command_env()`, `detect_agent_setup_failure()` |
| `crates/looper-agent/src/executor.rs`                 | `ConfiguredExecutor`, `Execution` lifecycle, timeout logic |
| `crates/looper-agent/src/parse.rs`                    | `parse_completion()`, `extract_native_session_id()` |
| `crates/looper-agent/src/error.rs`                    | `AgentError` enum                         |
| `crates/looper-agent/src/lib.rs`                      | Public re-exports                         |
