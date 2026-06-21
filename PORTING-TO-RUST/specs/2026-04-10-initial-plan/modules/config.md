# looperd 配置 Schema 详细实现计划

## 1. 目标

为 `looperd` 提供统一、可持久化、可校验的配置结构，覆盖：

- 服务监听
- 仓库与工作目录
- 轮询与并发
- 通知
- Agent 默认行为
- 外部工具路径
- daemon 运行时配置

---

## 2. 配置文件位置建议

优先级从高到低：

1. CLI 参数
2. 环境变量
3. `LOOPER_CONFIG` 指向的配置文件
4. 默认用户配置：`~/.looper/config.json`
5. 内置默认值

---

## 3. 顶层 Schema 建议

```ts
type LooperConfig = {
  server: ServerConfig
  storage: StorageConfig
  scheduler: SchedulerConfig
  agent: AgentConfig
  logging: LoggingConfig
  notifications: NotificationConfig
  tools: ToolPathsConfig
  daemon: DaemonConfig
  package: PackageConfig
  defaults: DefaultsConfig
  projects: ProjectRefConfig[]
}
```

---

## 4. 子配置建议

### 4.1 ServerConfig

```ts
type ServerConfig = {
  host: string
  port: number
  baseUrl?: string
  authMode: 'none' | 'local-token'
  localToken?: string
}
```

### 4.2 StorageConfig

```ts
type StorageConfig = {
  mode: 'sqlite'
  dbPath: string
  backupDir?: string
}
```

### 4.3 SchedulerConfig

```ts
type SchedulerConfig = {
  pollIntervalSeconds: number
  maxConcurrentRuns: number
  retryMaxAttempts: number
  retryBaseDelayMs: number
}
```

### 4.4 AgentConfig

```ts
type AgentConfig = {
  vendor: 'claude-code' | 'codex' | 'opencode' | 'cursor-cli'
  model?: string
  params?: Record<string, unknown>
  env?: Record<string, string>
}
```

### 4.5 NotificationConfig

```ts
type NotificationConfig = {
  inApp: boolean
  osascript: {
    enabled: boolean
    soundForLevels?: Array<'action_required' | 'failure'>
    throttleWindowSeconds: number
  }
}
```

### 4.6 LoggingConfig

```ts
type LoggingConfig = {
  level: 'debug' | 'info' | 'warn' | 'error'
  maxSizeMB: number
  maxFiles: number
}
```

### 4.7 ToolPathsConfig

```ts
type ToolPathsConfig = {
  bunPath?: string
  gitPath?: string
  ghPath?: string
  osascriptPath?: string
}
```

### 4.8 DaemonConfig

```ts
type DaemonConfig = {
  mode: 'foreground' | 'launchd'
  plistPath?: string
  logDir: string
  workingDirectory: string
  environment?: Record<string, string>
}
```

### 4.9 PackageConfig

```ts
type PackageConfig = {
  distribution: 'npm'
  autoMigrateOnStartup: boolean
  requireBackupBeforeMigrate: boolean
}
```

### 4.10 DefaultsConfig

```ts
type DefaultsConfig = {
  baseBranch: string
  allowAutoCommit: boolean
  allowAutoPush: boolean
  allowAutoApprove: boolean
  openPrStrategy?: 'all_done' | 'first_commit' | 'manual'
}
```

### 4.11 ProjectRefConfig

```ts
type ProjectRefConfig = {
  id: string
  name: string
  repoPath: string
  baseBranch?: string
  worktreeRoot?: string
}
```

Project 在 config 中是**静态真相源**，仅定义注册信息；运行态状态写入 storage。

---

## 5. 环境变量建议

- `LOOPER_CONFIG`
- `LOOPER_HOST`
- `LOOPER_PORT`
- `LOOPER_LOG_DIR`
- `LOOPER_DB_PATH`
- `LOOPER_BUN_PATH`
- `LOOPER_GIT_PATH`
- `LOOPER_GH_PATH`
- `LOOPER_OSASCRIPT_PATH`

在 npm 分发场景下，环境变量主要用于覆盖默认自动探测结果，而不是要求用户首次安装时手工配置全部路径。

---

## 6. 校验规则建议

- `port` 必须在合法范围内
- `dbPath`、`logDir`、`workingDirectory` 必须可写
- `agent.vendor` 必须是当前已接入的单一 agent 之一
- 当 `daemon.mode=launchd` 时，必须能解析出 `bunPath`
- 当 `notifications.osascript.enabled=true` 时，必须能解析出 `osascriptPath`
- `pollIntervalSeconds` 不能过小，建议最小 10 秒
- `logging.maxSizeMB` / `logging.maxFiles` 不能为 0
- `package.autoMigrateOnStartup` 在 npm 分发模式下应默认为 `true`

---

## 7. MVP 必须有的配置项

- `server.port`
- `storage.dbPath`
- `scheduler.pollIntervalSeconds`
- `logging.level`
- `notifications.osascript.enabled`
- `tools.gitPath`
- `tools.ghPath`
- `daemon.mode`
- `daemon.logDir`
- `package.autoMigrateOnStartup`

---

## 8. 与其他模块的关系

- `storage.dbPath` 对应 `storage.md`
- `notifications` 对应 `integrations.md`
- `daemon` 对应 `looperd.md`
- `agent` / `projects` 会和 `integrations.md`、`domain.md` 联动

---

## 9. MVP 决策结论

当前 spec 默认采用以下结论：

1. **单仓库优先**，多仓放到后续阶段
2. **默认全自动，但保留关键节点人工恢复/暂停能力**
3. **Review 通过以 GitHub Approve 为强信号**，reaction/comment 为弱信号
4. **Checklist 以存储实体为主**，可后续再与 markdown 同步
5. **GitHub 数据优先通过 gh cli**，后续如稳定性不足再切 API
6. **looperd 先面向本机运行**，launchd 为推荐 daemon 方案
