# Looper Configuration Reference

Looper uses a layered configuration system with TOML as the default format (YAML and JSON are also supported). The 3-layer merge pipeline resolves settings in this precedence order (highest wins):

1. **CLI overrides** (passed programmatically via `ConfigLoader::with_cli_overrides`)
2. **Environment variables** (`LOOPER_*` prefix)
3. **Config file** (auto-discovered or explicit path)
4. **Embedded defaults** (hard-coded in each config struct)

## Config File Discovery

Auto-discovery searches the following paths in order:

1. `$LOOPER_CONFIG` (environment variable, if set)
2. `$XDG_CONFIG_HOME/looper/` (if XDG_CONFIG_HOME is set)
3. Platform config directory (`~/.config/looper/` on Linux, `~/Library/Application Support/com.looper.looper/` on macOS)
4. `~/.looper/` (legacy)

Within each directory, config files are tried in order: `looper.toml`, `looper.yaml`, `looper.yml`, `looper.json`.

### Format Detection

Format is determined by file extension:

| Extension | Parser |
|-----------|--------|
| `.toml`   | TOML   |
| `.yaml`, `.yml` | YAML |
| `.json`   | JSON   |

If the extension is unrecognised, TOML is used as the fallback.

### Permissions Check

On Unix systems, the config loader warns if the config file is world-readable (mode looser than `0600`). See `looper-config/src/permissions.rs`.

---

## Top-Level Sections

```toml
[server]
# ...

[daemon]
# ...

[storage]
# ...

[scheduler]
# ...

[agent]
# ...

[logging]
# ...

[notifications]
# ...

[disclosure]
# ...

[tools]
# ...

[package]
# ...

[defaults]
# ...

[instructions]
# ...

[roles]
# ...

[[projects]]
# ...
```

---

## `[server]` -- HTTP Server

Controls the Looper API server binding and behavior.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `host` | string | `"127.0.0.1"` | Bind address. Use `"0.0.0.0"` for all interfaces. |
| `port` | integer (u16) | `7391` | TCP port. Must not be `0`. |
| `tls-cert` | string (optional) | `None` | Path to TLS certificate file for HTTPS. |
| `tls-key` | string (optional) | `None` | Path to TLS private key file. |
| `allowed-origins` | array of strings | `["http://localhost:7391"]` | CORS allowed origins. |
| `read-timeout-secs` | integer (u64) | `30` | Maximum time to wait for a full request body. |
| `write-timeout-secs` | integer (u64) | `60` | Maximum time to write a response. |
| `max-body-size-mb` | integer (u64) | `10` | Maximum accepted request body size. `0` disables uploads (warning). |
| `cors-enabled` | boolean | `true` | Whether CORS middleware is active. |

### Environment Variables

- `LOOPER_SERVER_HOST` -- overrides `host`
- `LOOPER_SERVER_PORT` -- overrides `port`

### Validation

- `port` must not be `0` (error).
- `host` must not be empty (error).
- `max-body-size-mb` of `0` emits a warning (uploads disabled).

### Example

```toml
[server]
host = "0.0.0.0"
port = 7391
cors-enabled = true
allowed-origins = ["https://app.looper.dev"]
```

---

## `[daemon]` -- Daemon / Service Manager

Controls how the Looper daemon runs and restarts.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `mode` | string (enum) | `"local"` | Daemon execution mode. See DaemonMode. |
| `pid-file` | string (optional) | `None` | Path to write the PID file. |
| `log-file` | string (optional) | `None` | Path to the daemon's own log file. |
| `max-restarts` | integer (u32) | `5` | Maximum number of automatic restarts. `0` disables restart (warning). |
| `restart-delay-secs` | integer (u64) | `5` | Delay between restart attempts. |
| `restart-policy` | string (enum) | `"always"` | When to restart. See DaemonRestartPolicy. |

### DaemonMode (from `looper-types`)

| Variant | String |
|---------|--------|
| `Local` | `"local"` |
| `Remote` | (remote mode) |
| `Hybrid` | (hybrid mode) |

### DaemonRestartPolicy

| Variant | String | Aliases |
|---------|--------|---------|
| `Always` | `"always"` | |
| `OnFailure` | `"on-failure"` | `"onfailure"` |
| `Never` | `"never"` | |

### Environment Variables

- `LOOPER_DAEMON_MODE` -- overrides `mode`
- `LOOPER_DAEMON_PID_FILE` -- overrides `pid-file`

### Validation

- `max-restarts` of `0` emits a warning (daemon will not restart).

### Example

```toml
[daemon]
mode = "local"
max-restarts = 10
restart-policy = "on-failure"
```

---

## `[storage]` -- Persistence Backend

Controls data storage for Looper's internal state.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `backend` | string (enum) | `"sqlite"` | Storage engine. See StorageBackend. |
| `path` | string (optional) | `None` | File path for SQLite databases. |
| `connection-string` | string (optional) | `None` | Full connection string (e.g., Postgres URI). |
| `pool-size` | integer (u32) | `4` | Connection pool size. Must be `>= 1`. |
| `timeout-secs` | integer (u64) | `30` | Query timeout. `0` may hang on a busy database (warning). |
| `migration-dir` | string (optional) | `None` | Directory containing SQL migration files. |
| `wal-enabled` | boolean | `true` | Enable WAL mode for SQLite. |
| `retry-on-busy` | boolean | `true` | Retry when the database reports busy. |

### StorageBackend

| Variant | String | Aliases |
|---------|--------|---------|
| `Sqlite` | `"sqlite"` | |
| `Postgres` | `"postgres"` | `"postgresql"` |
| `Memory` | `"memory"` | |

### Environment Variables

- `LOOPER_STORAGE_BACKEND` -- overrides `backend`
- `LOOPER_STORAGE_PATH` -- overrides `path`
- `LOOPER_STORAGE_CONNECTION_STRING` -- overrides `connection-string`

### Validation

- `pool-size` must be `>= 1` (error).
- `timeout-secs` of `0` emits a warning.

### Example

```toml
[storage]
backend = "sqlite"
path = "~/.local/share/looper/data.db"
pool-size = 8
wal-enabled = true
```

---

## `[scheduler]` -- Task Scheduler

Controls how the Looper scheduler dispatches and retries tasks.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `policy` | string (enum) | `"fifo"` | Queue scheduling policy. See SchedulerPolicy. |
| `max-workers` | integer (u32) | `4` | Max concurrent worker tasks. Must be `>= 1`. |
| `queue-capacity` | integer (u32) | `256` | Max pending tasks in queue. Must be `>= 1`. |
| `poll-interval-ms` | integer (u64) | `1000` | Queue polling interval in ms. `0` causes busy-loop (warning). |
| `claim-timeout-secs` | integer (u64) | `300` | Max time a worker can hold a claimed task. |
| `graceful-shutdown-secs` | integer (u64) | `30` | Time allowed for in-flight tasks to finish on shutdown. |

### SchedulerPolicy

| Variant | String | Aliases |
|---------|--------|---------|
| `Fifo` | `"fifo"` | |
| `Priority` | `"priority"` | |
| `RoundRobin` | `"round-robin"` | `"round_robin"` |
| `Weighted` | `"weighted"` | |

### `[scheduler.retry]` -- Retry Configuration

Controls exponential backoff for failed tasks.

```toml
[scheduler.retry]
max-attempts = 3
base-delay-secs = 5
max-delay-secs = 300
multiplier = 2.0
jitter = true
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max-attempts` | integer (u32) | `3` | Max retry attempts. `0` disables retries (warning). |
| `base-delay-secs` | integer (u64) | `5` | Initial delay before first retry. |
| `max-delay-secs` | integer (u64) | `300` | Maximum delay cap. |
| `multiplier` | float (f64) | `2.0` | Exponential multiplier applied each attempt. |
| `jitter` | boolean | `true` | Add random jitter to avoid thundering herd. |

The effective delay for attempt `n` (1-indexed) is roughly `min(base * multiplier^(n-1), max)`, plus optional jitter.

### Validation

- `max-workers` must be `>= 1` (error).
- `queue-capacity` must be `>= 1` (error).
- `poll-interval-ms` of `0` emits a warning (busy loop).
- `retry.max-attempts` of `0` emits a warning.
- `retry.base-delay-secs` of `0` emits a warning.

### Example

```toml
[scheduler]
policy = "fifo"
max-workers = 8
queue-capacity = 1024

[scheduler.retry]
max-attempts = 5
base-delay-secs = 10
max-delay-secs = 600
multiplier = 2.0
jitter = true
```

---

## `[agent]` -- LLM Agent Settings

Controls how Looper interacts with AI agent vendors.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `default-vendor` | string (enum) | `"claude"` | Primary agent vendor. See AgentVendor. |
| `timeout-secs` | integer (u64) | `120` | Agent API call timeout. Must be `> 0`. |
| `max-retries` | integer (u32) | `3` | API call retry count on failure. |
| `parallel-runs` | integer (u32) | `1` | Max parallel agent executions. `0` prevents execution (warning). |
| `model` | string (optional) | `None` | Vendor-specific model identifier override. |
| `temperature` | float (f64, optional) | `None` | LLM temperature override (0.0 -- 2.0 typically). |
| `max-tokens` | integer (u32, optional) | `None` | Max tokens in LLM response. |
| `allowed-vendors` | array of strings | `[]` | Whitelist of permitted vendors. Empty allows all. |

### AgentVendor (from `looper-types`)

| Variant | String |
|---------|--------|
| `Claude` | `"claude"` |
| `OpenAI` | `"openai"` |
| `Gemini` | `"gemini"` |

### Environment Variables

- `LOOPER_AGENT_DEFAULT_VENDOR` -- overrides `default-vendor`

### Validation

- `timeout-secs` of `0` is an error.
- `parallel-runs` of `0` emits a warning.

### Example

```toml
[agent]
default-vendor = "claude"
timeout-secs = 300
max-retries = 5
parallel-runs = 2
allowed-vendors = ["claude", "openai"]
```

---

## `[logging]` -- Logging Configuration

Controls log output format, level, and rotation.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `level` | string (enum) | `"info"` | Minimum log level. See LogLevel. |
| `format` | string (enum) | `"text"` | Log line format. See LogFormat. |
| `output` | string (enum) | `"stdout"` | Log destination. See LogOutput. |
| `file-path` | string (optional) | `None` | File path when `output = "file"`. |
| `max-files` | integer (u32) | `7` | Max rotated log files to retain. `0` with file output = unlimited growth (warning). |
| `max-size-mb` | integer (u64) | `100` | Max size per log file before rotation. `0` = unbounded (warning). |
| `rotation` | string (enum) | `"daily"` | Log rotation strategy. See LogRotation. |

### LogLevel (from `looper-types`)

| Variant | String |
|---------|--------|
| `Trace` | `"trace"` |
| `Debug` | `"debug"` |
| `Info` | `"info"` |
| `Warn` | `"warn"` |
| `Error` | `"error"` |

### LogFormat

| Variant | String |
|---------|--------|
| `Text` | `"text"` |
| `Json` | `"json"` |
| `Compact` | `"compact"` |

### LogOutput

| Variant | String |
|---------|--------|
| `Stdout` | `"stdout"` |
| `Stderr` | `"stderr"` |
| `File` | `"file"` |
| `Syslog` | `"syslog"` |

### LogRotation

| Variant | String | Aliases |
|---------|--------|---------|
| `Daily` | `"daily"` | |
| `Hourly` | `"hourly"` | |
| `SizeBased` | `"size-based"` | `"size_based"` |
| `Never` | `"never"` | |

### Environment Variables

- `LOOPER_LOG_LEVEL` -- overrides `level`
- `LOOPER_LOG_FORMAT` -- overrides `format`
- `LOOPER_LOG_FILE` -- overrides `file-path`

### Validation

- `max-files` of `0` with `output = "file"` emits a warning (unlimited growth).
- `max-size-mb` of `0` emits a warning (unbounded log file).

### Example

```toml
[logging]
level = "debug"
format = "json"
output = "file"
file-path = "/var/log/looper/looper.log"
max-files = 14
max-size-mb = 200
rotation = "size-based"
```

---

## `[notifications]` -- Notification Dispatch

Controls how Looper sends notifications about events.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | boolean | `true` | Master switch for notifications. |
| `min-priority` | string (enum) | `"normal"` | Minimum priority to dispatch. See NotificationPriority. |
| `channels` | array of strings | `["log"]` | Active notification channels. See NotificationChannel. |
| `webhook-url` | string (optional) | `None` | Generic webhook URL. |
| `slack-webhook` | string (optional) | `None` | Slack incoming webhook URL. |
| `email-smtp` | string (optional) | `None` | SMTP connection string for email. |
| `desktop-notifications` | boolean | `false` | Enable OS-level desktop notifications. |

### NotificationPriority

| Variant | String |
|---------|--------|
| `Low` | `"low"` |
| `Normal` | `"normal"` |
| `High` | `"high"` |
| `Urgent` | `"urgent"` |

### NotificationChannel

| Variant | String |
|---------|--------|
| `Log` | `"log"` |
| `Desktop` | `"desktop"` |
| `Email` | `"email"` |
| `Webhook` | `"webhook"` |
| `Slack` | `"slack"` |

### Environment Variables

- `LOOPER_NOTIFICATIONS_ENABLED` -- overrides `enabled` (accepts `"true"` / `"1"`)
- `LOOPER_NOTIFICATIONS_WEBHOOK_URL` -- overrides `webhook-url`

### Validation

- If `enabled = true` and `channels` is empty, a warning is emitted.

### Example

```toml
[notifications]
enabled = true
min-priority = "high"
channels = ["log", "desktop", "slack"]
slack-webhook = "https://hooks.slack.com/services/..."
```

---

## `[disclosure]` -- AI-Generated Content Disclosure

Controls how Looper stamps AI-generated output (commit messages, PR descriptions, review comments).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `format` | string (enum) | `"markdown"` | Disclosure output format. See DisclosureFormat. |
| `include-code` | boolean | `true` | Include code blocks in disclosure output. |
| `include-diffs` | boolean | `true` | Include diffs in disclosure output. |
| `include-metadata` | boolean | `true` | Include metadata (timestamps, hashes) in disclosure output. |
| `protected-phrases` | array of strings | `[]` | Phrases that trigger content redaction (case-insensitive match). |
| `max-length` | integer (u64, optional) | `None` | Max disclosure body length. `0` suppresses all disclosure (warning). |

### DisclosureFormat

| Variant | String | Aliases |
|---------|--------|---------|
| `Markdown` | `"markdown"` | `"md"` |
| `Html` | `"html"` | |
| `Text` | `"text"` | |

### `[disclosure.stamp]` -- Stamp Configuration

Controls the AI-generated stamp appended to output.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | boolean | `true` | Master switch for stamp insertion. |
| `prefix` | string | `"ai-generated"` | Stamp identifier text. |
| `include-timestamp` | boolean | `true` | Embed a Unix timestamp in stamp. |
| `include-config-hash` | boolean | `true` | Embed a config hash in stamp. |

The stamp format varies by disclosure format:

- **Markdown / HTML**: `<!-- ai-generated; ts=1234567890; config=<hash> -->`
- **Text**: `ai-generated; ts=1234567890; config=<hash>`

### Stamp API

The disclosure module (`disclosure.rs`) provides these functions:

- `generate_stamp(config)` -- Build a disclosure stamp from config (returns `None` if disabled).
- `has_stamp(text, config)` -- Check if text already contains a stamp.
- `strip_stamps(text, config)` -- Remove stamps from text, returning cleaned text and a boolean flag.
- `check_protected_phrases(text, config)` -- Find matching protected phrases in text (case-insensitive).

### Validation

- `max-length` of `0` emits a warning (all disclosure suppressed).

### Example

```toml
[disclosure]
format = "markdown"
include-code = true
include-diffs = true
include-metadata = true
protected-phrases = ["api-key", "secret"]
max-length = 10000

[disclosure.stamp]
enabled = true
prefix = "ai-generated"
include-timestamp = true
include-config-hash = true
```

---

## `[tools]` -- Tool Execution Environment

Controls how Looper executes subprocess tools.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `runtime` | string (enum) | `"host"` | Execution environment. See ToolRuntime. |
| `allow-network` | boolean | `true` | Allow tools to make network requests. |
| `timeout-secs` | integer (u64) | `300` | Per-tool execution timeout. Must be `> 0`. |
| `cache-enabled` | boolean | `true` | Cache tool outputs for reuse. |
| `cache-dir` | string (optional) | `None` | Tool cache directory. |
| `allowed-paths` | array of strings | `[]` | Filesystem paths tools may access (empty = all allowed). |
| `denied-paths` | array of strings | `[]` | Filesystem paths tools may NOT access. |

### ToolRuntime

| Variant | String |
|---------|--------|
| `Host` | `"host"` |
| `Docker` | `"docker"` |
| `Nix` | `"nix"` |
| `Container` | `"container"` |

### `[tools.docker]` -- Docker Runtime Configuration

Only relevant when `runtime = "docker"`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `image` | string | `"looper/tools"` | Docker image name. |
| `tag` | string | `"latest"` | Image tag. |
| `volumes` | array of strings | `[]` | Volume mount specs (`/host:/container`). |
| `network` | string | `"host"` | Docker network mode. |

### `[tools.nix]` -- Nix Runtime Configuration

Only relevant when `runtime = "nix"`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `packages` | array of strings | `[]` | Nix packages to make available in the environment. |
| `extra-nix-path` | string (optional) | `None` | Extra `-I` nix path expression. |

### Validation

- `timeout-secs` of `0` is an error.

### Example

```toml
[tools]
runtime = "host"
allow-network = true
timeout-secs = 600
cache-enabled = true
cache-dir = "~/.cache/looper/tools"
denied-paths = ["/etc/shadow", "/home/*/.ssh"]

[tools.docker]
image = "looper/tools"
tag = "v2.1"
volumes = ["/data:/data"]
network = "bridge"
```

---

## `[package]` -- Package Identity

Metadata about the Looper package itself.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | `"looper"` | Package name. |
| `version` | string | `"0.1.0"` | Package version (semver). |
| `registry` | string (optional) | `None` | Package registry URL for updates. |
| `install-dir` | string (optional) | `None` | Installation directory path. |

### Example

```toml
[package]
name = "looper"
version = "1.2.0"
registry = "https://registry.looper.dev"
```

---

## `[defaults]` -- Directory and Shell Defaults

Platform-aware default paths and user preferences. All fields are optional; when set, they override platform-detected defaults.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `home-dir` | string (optional) | Platform home | Override home directory path. |
| `config-dir` | string (optional) | Platform config | Override config directory (`~/.config/looper`). |
| `data-dir` | string (optional) | Platform data | Override data directory. |
| `cache-dir` | string (optional) | Platform cache | Override cache directory. |
| `log-dir` | string (optional) | Platform logs | Override log directory. |
| `editor` | string (optional) | `$EDITOR` | Default text editor command. |
| `shell` | string (optional) | `$SHELL` | Default shell command. |

### Platform Defaults

Derived from `directories::ProjectDirs` for `com.looper.looper`:

| OS | Config | Data | Cache | Log |
|----|--------|------|-------|-----|
| Linux | `~/.config/looper` | `~/.local/share/looper` | `~/.cache/looper` | `~/.local/share/looper/log` |
| macOS | `~/Library/Application Support/com.looper.looper` | (same) | `~/Library/Caches/com.looper.looper` | `~/Library/Logs/com.looper.looper` |
| Windows | `~/AppData/Roaming/com.looper/looper/config` | `~/AppData/Roaming/com.looper/looper/data` | `~/AppData/Local/com.looper/looper/cache` | `~/AppData/Roaming/com.looper/looper/data/log` |

### Example

```toml
[defaults]
editor = "nvim"
shell = "/bin/zsh"
config-dir = "/etc/looper"
```

---

## `[instructions]` -- System Instructions

Controls how Looper loads and serves system prompt instructions for agents.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `base-path` | string (optional) | `None` | Root directory for instruction files. |
| `encoding` | string | `"utf-8"` | Text encoding for instruction files. |
| `max-size-kb` | integer (u64) | `1024` | Maximum instruction file size. `0` rejects all instructions (warning). |
| `allowed-extensions` | array of strings | `[".md", ".txt"]` | Permitted file extensions for instruction files. |

### Validation

- `max-size-kb` of `0` emits a warning (all instructions rejected).

### Example

```toml
[instructions]
base-path = "/etc/looper/instructions"
encoding = "utf-8"
max-size-kb = 512
allowed-extensions = [".md", ".txt", ".adoc"]
```

---

## `[roles]` -- Role Configuration

Defines per-role overrides for the agent system. Each role inherits from the top-level `[agent]` section unless overridden here.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `planner` | table | (see RoleConfig) | Planner agent role settings. |
| `reviewer` | table | (see RoleConfig) | Reviewer agent role settings. |
| `worker` | table | (see RoleConfig) | Worker agent role settings. |
| `fixer` | table | (see RoleConfig) | Fixer agent role settings. |
| `coordinator` | table | (see RoleConfig) | Coordinator agent role settings. |

### `<role>.RoleConfig`

Each of the five roles supports the same fields:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | boolean | `true` | Whether this role is active. |
| `model` | string (optional) | `None` | Model override for this role. |
| `instructions-path` | string (optional) | `None` | Role-specific instructions file. |
| `timeout-secs` | integer (u64) | `300` | Per-role timeout. `0` on an enabled role emits a warning. |
| `max-retries` | integer (u32) | `3` | Per-role retry limit. |
| `parallel-tasks` | integer (u32) | `1` | Per-role parallel task limit. |
| `max-tokens` | integer (u32, optional) | `None` | Per-role max response tokens. |
| `temperature` | float (f64, optional) | `None` | Per-role temperature override. |

### Validation

- If a role is `enabled` but has `timeout-secs = 0`, a warning is emitted.

### Example

```toml
[roles]
[roles.planner]
enabled = true
model = "claude-sonnet-4-20250514"
timeout-secs = 600
max-tokens = 8192

[roles.reviewer]
enabled = true
model = "claude-sonnet-4-20250514"
temperature = 0.3

[roles.worker]
enabled = true
parallel-tasks = 3
timeout-secs = 120

[roles.fixer]
enabled = true
timeout-secs = 300

[roles.coordinator]
enabled = true
model = "claude-opus-4-20250514"
```

---

## `[[projects]]` -- Project Definitions

Multiple projects can be defined as an array of tables. Each entry defines a Looper workspace.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | `""` | Project display name. Must not be empty. |
| `path` | string (optional) | `None` | Absolute or relative path to the project directory. |
| `default-loop-type` | string (optional) | `None` | Default loop command for this project. |
| `schedule` | string (optional) | `None` | Cron-style schedule expression. |
| `enabled` | boolean | `true` | Whether this project is active. |

### Validation

- `name` must not be empty (error).

### Example

```toml
[[projects]]
name = "looper-core"
path = "/home/user/looper"
default-loop-type = "dev"
schedule = "0 9 * * 1-5"

[[projects]]
name = "docs"
path = "/home/user/looper/docs"
enabled = false
```

---

## Enum Reference

All configuration enums are defined in `looper-config/src/enums.rs`. They use case-insensitive parsing and support the aliases shown below.

| Enum | Default | Variants |
|------|---------|----------|
| `DaemonRestartPolicy` | `"always"` | `"always"`, `"on-failure"` / `"onfailure"`, `"never"` |
| `ToolRuntime` | `"host"` | `"host"`, `"docker"`, `"nix"`, `"container"` |
| `SchedulerPolicy` | `"fifo"` | `"fifo"`, `"priority"`, `"round-robin"` / `"round_robin"`, `"weighted"` |
| `StorageBackend` | `"sqlite"` | `"sqlite"`, `"postgres"` / `"postgresql"`, `"memory"` |
| `LogFormat` | `"text"` | `"text"`, `"json"`, `"compact"` |
| `LogOutput` | `"stdout"` | `"stdout"`, `"stderr"`, `"file"`, `"syslog"` |
| `LogRotation` | `"daily"` | `"daily"`, `"hourly"`, `"size-based"` / `"size_based"`, `"never"` |
| `NotificationPriority` | `"normal"` | `"low"`, `"normal"`, `"high"`, `"urgent"` |
| `NotificationChannel` | `"log"` | `"log"`, `"desktop"`, `"email"`, `"webhook"`, `"slack"` |
| `DisclosureFormat` | `"markdown"` | `"markdown"` / `"md"`, `"html"`, `"text"` |

Additional enums defined but not directly referenced by the config struct:

| Enum | Default | Variants |
|------|---------|----------|
| `AgentMode` | `"auto"` | `"auto"`, `"manual"`, `"supervised"` |
| `OpenPRStrategy` | `"create"` | `"create"`, `"update"`, `"skip"` |
| `AddSnapshotMode` | `"none"` | `"none"`, `"all"`, `"head"` |
| `ReviewerScope` | `"changed"` | `"changed"`, `"full"`, `"smart"` |
| `FixApplyMode` | `"direct"` | `"direct"`, `"branch"`, `"draft"` |
| `SchedulePeriod` | `"hour"` | `"minute"` / `"min"`, `"hour"` / `"hr"`, `"day"`, `"week"`, `"month"` |
| `DiffAlgorithm` | `"histogram"` | `"histogram"`, `"patience"`, `"myers"` |
| `RetryBackoff` | `"constant"` | `"constant"`, `"exponential"` |
| `AuthMode` | `"token"` | `"token"`, `"basic"`, `"oauth"` / `"oauth2"`, `"none"` |
| `ServerProtocol` | `"http"` | `"http"`, `"unix"` |

---

## Environment Variable Reference

All environment variables use the prefix `LOOPER_` with screaming snake case section and field names.

| Variable | Overrides | Type |
|----------|-----------|------|
| `LOOPER_SERVER_HOST` | `server.host` | string |
| `LOOPER_SERVER_PORT` | `server.port` | integer |
| `LOOPER_DAEMON_MODE` | `daemon.mode` | string (enum) |
| `LOOPER_DAEMON_PID_FILE` | `daemon.pid-file` | string |
| `LOOPER_STORAGE_BACKEND` | `storage.backend` | string (enum) |
| `LOOPER_STORAGE_PATH` | `storage.path` | string |
| `LOOPER_STORAGE_CONNECTION_STRING` | `storage.connection-string` | string |
| `LOOPER_LOG_LEVEL` | `logging.level` | string (enum) |
| `LOOPER_LOG_FORMAT` | `logging.format` | string (enum) |
| `LOOPER_LOG_FILE` | `logging.file-path` | string |
| `LOOPER_NOTIFICATIONS_ENABLED` | `notifications.enabled` | boolean (`"true"` / `"1"`) |
| `LOOPER_NOTIFICATIONS_WEBHOOK_URL` | `notifications.webhook-url` | string |

---

## Validation Summary

The `validate_config()` function in `validate.rs` checks all resolved fields and returns a `ConfigValidation` containing errors and warnings. The config is rejected if any errors are present; warnings are advisory only.

| Path | Condition | Severity |
|------|-----------|----------|
| `server.port` | == 0 | Error |
| `server.host` | empty | Error |
| `server.max-body-size-mb` | == 0 | Warning |
| `daemon.max-restarts` | == 0 | Warning |
| `storage.pool-size` | == 0 | Error |
| `storage.timeout-secs` | == 0 | Warning |
| `scheduler.max-workers` | == 0 | Error |
| `scheduler.queue-capacity` | == 0 | Error |
| `scheduler.poll-interval-ms` | == 0 | Warning |
| `scheduler.retry.max-attempts` | == 0 | Warning |
| `scheduler.retry.base-delay-secs` | == 0 | Warning |
| `agent.timeout-secs` | == 0 | Error |
| `agent.parallel-runs` | == 0 | Warning |
| `logging.max-files` | == 0 with file output | Warning |
| `logging.max-size-mb` | == 0 | Warning |
| `notifications.channels` | empty when enabled | Warning |
| `disclosure.max-length` | == 0 | Warning |
| `tools.timeout-secs` | == 0 | Error |
| `instructions.max-size-kb` | == 0 | Warning |
| `roles.<name>.timeout-secs` | == 0 when enabled | Warning |
| `projects[N].name` | empty | Error |

---

## Merge Pipeline (PartialConfig)

All config structs have corresponding `Partial*` types where every field is `Option<T>`. The merge pipeline layers partial configs from multiple sources using the `Merge` trait:

- `Option<T>`: merged by replacement if `other` is `Some`
- `Vec<T>`: merged by extension
- `String`: merged by replacement if `other` is non-empty
- Nested structs: field-by-field recursive merge

The final `PartialConfig` is then converted to a fully-resolved `Config` via `From<PartialConfig>`, where any `None` field falls through to the struct's `Default` implementation (the embedded defaults shown throughout this document).

---

## Programmatic Loading

```rust
use looper_config::loader::ConfigLoader;

let config = ConfigLoader::new()
    .with_config_path("/etc/looper/looper.toml".into())
    .load()
    .expect("valid config");

println!("Server port: {}", config.server.unwrap().port);
```

The `ConfigLoader` builder also supports:
- `with_cli_overrides(PartialConfig)` -- highest-precedence overrides
- `skip_env()` -- disable environment variable loading
- `skip_file()` -- disable file loading (for testing or embedded configs)
