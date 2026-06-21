# looperd 详细实现计划

## 1. 目标

`looperd` 是 Looper 的执行中心，负责：

- 启动与关闭生命周期
- HTTP API
- Loop 调度
- 状态持久化
- 外部系统调用
- 审计与恢复

---

## 2. 目录建议

```txt
apps/looperd/
  src/
    server/
    app/
    domain/
    hooks/
    infra/
    storage/
    bootstrap/
```

---

## 3. 模块拆分

### 3.1 bootstrap

负责：

- 加载配置
- 初始化 logger
- 初始化 storage
- 初始化 adapters
- 启动 scheduler
- 启动 HTTP server

补充：`HookBus` 不是 MVP 启动前置；只有在实现对应抽象时才初始化。

### 3.2 server

负责：

- 路由注册
- 请求校验
- 鉴权（MVP 可先留空或本机限制）
- 错误映射

### 3.3 app

负责用例编排，例如：

- `CreateTaskService`
- `StartReviewerLoopService`
- `HandlePRReviewService`
- `FixOpenPRService`

### 3.4 domain

负责：

- 实体
- 值对象
- 状态机
- 领域规则

### 3.5 infra

负责：

- GitHub adapter
- Agent adapter
- Git/worktree adapter
- Notification adapter

### 3.6 hooks（后续可选）

负责：

- 生命周期事件订阅
- 审计日志落盘
- 通知分发
- metrics / trace / debug stream
- hook 错误隔离与开关控制

---

## 4. 启动顺序

1. 读取配置
2. 初始化存储
3. 做恢复逻辑
4. 初始化 adapters
5. 如已实现，则初始化 HookBus 与 process registry
6. 启动 scheduler
7. 启动 HTTP server
8. 写入 `looperd.started` 事件

关闭顺序相反。

---

## 4.1 开发阶段运行流程

适用于：

- 本地开发
- 调试 loop / agent / scheduler
- schema 变更验证

推荐流程：

1. 开发者在仓库内直接启动 `looperd` foreground 模式
2. 读取本地开发配置（优先 CLI / env，其次 `~/.looper/config.json`）
3. 自动执行 SQLite migration 检查
4. 初始化本地日志、db、queue、notifications
5. 启动 HTTP server
6. 启动 scheduler
7. CLI / UI 通过本地 API 调试
8. 修改代码后手动重启进程验证

开发阶段特点：

- foreground 优先
- 强调可观察性与快速重启
- 允许直接查看日志与 db 文件
- launchd 不作为必须前置条件

## 4.2 npm 分发阶段运行流程

适用于：

- 用户通过 `npm install -g looper` 安装后首次使用
- 用户升级 npm 包后的首次启动
- launchd 托管下的日常运行

推荐流程：

1. 用户通过 CLI 触发 `looper status` / `looper daemon start` / `looper daemon install`
2. CLI 检查配置、工具路径、daemon 状态
3. 如 `looperd` 尚未运行，则启动或交给 launchd 拉起
4. `looperd` 启动时自动执行 first-run boot flow：
   - 检查目录
   - 打开 SQLite
   - 自动 migration
   - 恢复 queue / locks / interrupted runs
   - 启动 HTTP server / scheduler
5. CLI 再通过 `/api/v1/status` 聚合展示系统状态

npm 分发阶段特点：

- 自动升级优先
- daemon 运行优先
- 用户不需要手工执行数据库升级
- 启动后优先通过统一 status 接口判断系统是否可用

---

## 5. 恢复逻辑

启动时必须执行统一恢复 pipeline：

1. 清理 orphan agent process
2. 清理过期锁
3. 标记中断的 run
4. 恢复未完成 loop 的下一次调度时间
5. 检查 worktree 映射是否还存在
6. 写恢复审计日志

失败策略：

- `orphan process` 清理失败：记 warning，允许继续启动
- `过期锁` / `中断 run` / `loop 状态恢复` 失败：阻塞启动
- `审计日志` 写失败：记 warning，允许继续启动

说明：如果 `looperd` 崩溃时仍有 agent 子进程存活，恢复阶段必须优先处理 orphan process，避免后台继续修改 worktree。

---

## 6. 关键接口

```ts
interface LooperdRuntime {
  start(): Promise<void>
  stop(): Promise<void>
}
```

```ts
interface RuntimeDeps {
  config: AppConfig
  stores: LooperStore
  scheduler: Scheduler
  hookBus?: HookBus
  github: GitHubGateway
  agents: AgentRegistry
  git: GitGateway
  notifications: NotificationGateway
}
```

---

## 7. MVP 里程碑

### Phase 1

- 本地启动成功
- 健康检查接口
- 基础配置加载
- SQLite 初始化 + migration runner

### Phase 2

- 接入 scheduler
- 能创建 task / loop / run
- 能调用一个 agent adapter

### Phase 3

- 恢复逻辑
- 审计日志
- 失败重试

---

## 8. 进程管理

## 8.1 结论

`looperd` 作为本地常驻后台服务，推荐在 macOS 上通过 **launchd** 管理。

但落地顺序建议是：

- **开发期 / Day 1**：先保留 foreground 模式，方便调试
- **第一阶段后期或第二阶段前期**：补上 launchd 集成，作为推荐运行方式

也就是说，launchd 很适合作为产品运行方式，但不应阻塞核心 loop 逻辑的先行实现。

## 8.2 两种运行模式

### foreground

用于：

- 本地开发
- 调试
- 快速排障

特点：

- 直接终端启动
- 日志直出
- 修改后重启简单

### daemon

用于：

- 用户日常使用
- 开机自启
- 崩溃自动恢复

推荐由 launchd 托管。

## 8.3 为什么用 launchd

相比手动 `bun run` / `tmux` / PM2，launchd 更适合当前形态：

- 原生支持开机自启
- 原生支持崩溃自动拉起
- 无需额外安装 Node 进程管理器
- 与 macOS `osascript` 通知同属本机系统能力
- 更符合 `looperd` 的 daemon 定位

## 8.4 关键约束

采用 launchd 时必须注意：

1. 不能依赖用户 shell 的 PATH
2. `bun` / `git` / `gh` 路径必须显式传入
3. 工作目录必须显式配置
4. 日志路径必须显式配置
5. 即使使用 launchd，也必须保留 foreground 模式

## 8.5 launchd 集成建议

### plist 标识与位置

- Label：`com.looper.looperd`
- 安装位置：`~/Library/LaunchAgents/com.looper.looperd.plist`

### 推荐 plist 字段

- `RunAtLoad = true`
- `KeepAlive = { SuccessfulExit = false }`
- `ProcessType = Background`
- `ThrottleInterval = 5`
- `WorkingDirectory = <looper-home-or-project-root>`
- `StandardOutPath = ~/.looper/logs/looperd.stdout.log`
- `StandardErrorPath = ~/.looper/logs/looperd.stderr.log`
- `EnvironmentVariables`
  - `PATH`
  - `BUN_INSTALL_BIN`（如需要）
  - 其他运行时必需变量

### ProgramArguments 建议

建议由 CLI 动态生成，避免硬编码路径，例如：

```xml
<array>
  <string>/absolute/path/to/bun</string>
  <string>run</string>
  <string>/absolute/path/to/apps/looperd/src/index.ts</string>
</array>
```

更推荐由 `looper daemon install` 根据当前环境解析出绝对路径，而不是静态分发 plist。

### plist 示例

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key>
    <string>com.looper.looperd</string>

    <key>ProgramArguments</key>
    <array>
      <string>/opt/homebrew/bin/bun</string>
      <string>run</string>
      <string>/Users/mrc/Projects/looper/apps/looperd/src/index.ts</string>
    </array>

    <key>RunAtLoad</key>
    <true/>

    <key>KeepAlive</key>
    <dict>
      <key>SuccessfulExit</key>
      <false/>
    </dict>

    <key>ProcessType</key>
    <string>Background</string>

    <key>ThrottleInterval</key>
    <integer>5</integer>

    <key>WorkingDirectory</key>
    <string>/Users/mrc/.looper</string>

    <key>EnvironmentVariables</key>
    <dict>
      <key>PATH</key>
      <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
      <key>LOOPER_CONFIG</key>
      <string>/Users/mrc/.looper/config.json</string>
    </dict>

    <key>StandardOutPath</key>
    <string>/Users/mrc/.looper/logs/looperd.stdout.log</string>

    <key>StandardErrorPath</key>
    <string>/Users/mrc/.looper/logs/looperd.stderr.log</string>
  </dict>
</plist>
```

注意：

- 上述路径仅为示例
- 实际 plist 必须由 `looper daemon install` 动态生成
- Bun、repo、配置、日志路径都需要按本机环境解析

### crash loop 防护

- 连续 N 次短时间启动失败后，`looperd` 应主动以成功退出结束，避免 launchd 无限拉起
- 同时写入错误日志与本地系统通知

## 8.6 CLI 命令建议

建议新增：

- `looper daemon install`
- `looper daemon uninstall`
- `looper daemon start`
- `looper daemon stop`
- `looper daemon restart`
- `looper daemon status`
- `looper daemon logs`

职责建议：

- `install`：生成并注册 plist
- `uninstall`：卸载并删除 plist
- `start/stop/restart`：封装 `launchctl`
- `status`：同时检查 launchd 状态 + `GET /healthz`
- `logs`：查看 stdout/stderr 日志

### 命令细化

#### `looper daemon install`

职责：

- 检测 `bun` 绝对路径
- 检测 `gh` / `git` / `osascript` 可执行路径
- 创建 `~/.looper`、`~/.looper/logs`
- 生成 plist
- 执行注册

建议参数：

- `--config <path>`
- `--bun <path>`
- `--force`
- `--dry-run`

#### `looper daemon uninstall`

职责：

- 停止 launchd job
- 删除 plist
- 保留日志与数据目录

建议参数：

- `--purge-plist-only`

#### `looper daemon start|stop|restart`

职责：

- 操作 launchd job
- `restart` = stop + start

#### `looper daemon status`

输出建议包含：

- launchd loaded / unloaded
- 进程 pid
- 最近启动时间
- `healthz` 状态
- 配置路径
- 日志路径

#### `looper daemon logs`

建议参数：

- `--stdout`
- `--stderr`
- `--follow`
- `--lines <n>`

## 8.7 落地顺序

### 第一阶段前期

- 只支持 foreground
- 通过 HTTP `healthz` 判断服务是否正常

### 第一阶段后期

- 增加 `daemon install/start/stop/status`
- 支持动态生成 plist
- 补日志目录自动创建

### 第二阶段

- 完整支持 daemon 生命周期管理
- 更好的状态诊断与错误提示

## 8.8 非目标

当前阶段先不做：

- Linux `systemd` 支持
- Docker 化 daemon 管理
- 多实例 looperd 管理

未来如需跨平台，再抽象 `DaemonManager`：

- `LaunchdDaemonManager`
- `SystemdDaemonManager`

---

## 9. npm 分发与自动升级

## 9.1 分发前提

Looper 预期通过 npm 分发，因此必须假设：

- 用户会通过 `npm install -g` 或 `npx` 使用 CLI
- 版本升级可能直接替换本地安装的 package 文件
- 已安装版本遗留的 SQLite 数据库必须被自动兼容升级

## 9.2 自动 migration 原则

- migration 不依赖人工单独执行
- 每次 `looperd` 启动时自动检查 schema 版本
- 发现有 pending migrations 时，先备份再执行
- migration 成功后才进入 HTTP 服务与 scheduler 启动阶段
- migration 失败则阻止服务进入 running 状态

## 9.3 推荐升级流程

### first-run boot flow（首次安装 / 首次升级启动）

适用于以下场景：

- 用户首次 `npm install -g looper`
- 用户升级到新版本后首次启动
- launchd 托管下的新版本首次拉起

统一流程建议：

1. 解析 CLI / env / config，确定 `dbPath`、`logDir`、工具路径
2. 检查 `~/.looper/`、日志目录、备份目录是否存在；不存在则自动创建
3. 打开 SQLite 连接
4. 读取当前 schema 版本与 pending migrations
5. 如果是首次安装：
   - 创建空数据库
   - 执行全部初始 migrations
6. 如果是升级后首次启动：
   - 先自动备份当前 db
   - 顺序执行所有 pending migrations
7. migration 成功后：
   - 写启动日志
   - 继续恢复 locks / queue / interrupted runs
   - 启动 HTTP server
   - 启动 scheduler
8. migration 失败后：
   - 服务进入 failed state
   - 不启动 HTTP server / scheduler
   - 输出明确错误与失败的 migration 文件名
   - 保留备份文件供回滚排查

### first-run 可观测性

首次启动或升级启动时，建议明确输出：

- package version
- db path
- current schema version
- target schema version
- pending migrations 数量
- backup 文件路径
- 最终启动结果

### 场景 A：用户升级 npm 包后首次启动

1. 用户升级 looper CLI / looperd 包
2. 执行 `looper daemon start` 或 launchd 自动拉起 `looperd`
3. `looperd` 启动时自动执行 migration 检查
4. 如有未执行 migration：
   - 备份 `looper.db`
   - 顺序执行 SQL migration
   - 更新 `schema_migrations`
5. 成功后继续启动 API / scheduler

### 场景 B：launchd 管理下自动升级后重启

1. npm 升级完成
2. 用户执行 `looper daemon restart` 或下次系统拉起
3. 启动流程同上

## 9.4 版本兼容约束

- npm package version 与 schema version 不要求一一对应
- 但每个发布版本必须携带完整 migration 集合
- 新版本不得依赖用户手工运行“升级数据库”命令

## 9.5 CLI 行为建议

建议新增：

- `looper doctor`：检查 bun / git / gh / osascript / db / migration 状态
- `looper db status`：显示当前 schema 版本、pending migrations、db 路径
- `looper db backup`：手工备份数据库

## 9.6 launchd 下的注意事项

- launchd 启动时也必须能完成 migration
- migration 期间不应启动 HTTP server 对外服务
- 若 migration 失败，daemon status 应能明确显示失败原因与失败的 migration 文件
