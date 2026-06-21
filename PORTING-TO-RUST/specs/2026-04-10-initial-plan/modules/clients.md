# CLI / UI 详细实现计划

## 1. 目标

CLI 和 UI 都只作为 `looperd` 的客户端，不直接操作 GitHub、Git、Agent。

---

## 2. CLI 实现计划

### 2.1 核心命令

- `looper status`
- `looper project add`
- `looper config show`
- `looper daemon install`
- `looper daemon status`
- `looper daemon logs`
- `looper loop list`
- `looper loop start`
- `looper loop pause`
- `looper task create`
- `looper task start`
- `looper task pause`
- `looper task status`
- `looper task show`
- `looper pr list`
- `looper pr show`
- `looper pr status`
- `looper run list`
- `looper logs tail`

说明：

- `task` 是常规用户主入口
- `pr` 是协作与交付的高频查看入口
- `loop start --type reviewer|fixer` 主要用于“已有 PR 托管”场景
- 常规 task 流程中，不应要求用户手动编排 worker / reviewer / fixer 三个 loop

推荐心智：

- `task`：开发入口
- `pr`：协作 / 交付入口
- `loop`：高级 / 调试入口

### 2.2 CLI 分层

- command parser
- api client
- output formatter

### 2.3 输出策略

- 默认人类可读
- 提供 `--json`
- 错误统一输出 request id / run id

### 2.4 `looper status` 命令建议

`looper status` 用于聚合展示各模块状态，避免用户分别调用 daemon / db / loop / queue 多个命令。

建议展示：

- looperd 服务状态
- daemon 状态（如适用）
- SQLite db 路径、schema 版本、pending migrations
- scheduler 状态与队列长度
- reviewer / worker / fixer 的运行摘要
- 外部工具可用性（bun / git / gh / osascript）
- 通知开关状态

建议参数：

- `--json`
- `--watch`
- `--verbose`

人类可读模式建议优先输出红/黄/绿摘要，JSON 模式则直接返回聚合状态原文。

### 2.5 daemon 命令建议

- `looper daemon install`：安装并注册 launchd plist
- `looper daemon start|stop|restart`：管理后台服务
- `looper daemon status`：展示 launchd + healthz 双状态
- `looper daemon logs`：查看 looperd stdout/stderr

推荐让 daemon 命令既输出 launchd 信息，也输出 `looperd` 自身健康状态，避免只看到“已加载”但服务实际上不可用。

### 2.6 config 命令建议

- `looper config show`：查看当前生效配置
- MVP 阶段不提供 agent profile / binding CRUD

### 2.7 PR 命令建议

- `looper pr list`：列出当前项目中用户最关心的 PR 状态
- `looper pr show <repo>#<number>`：查看某个 PR 的完整摘要
- `looper pr status <repo>#<number>`：查看某个 PR 当前是否在 review / fix / blocked

`looper pr list` 建议支持：

- `--active`
- `--blocked`
- `--needs-review`
- `--needs-fix`
- `--json`

建议展示：

- PR 标识（repo / number / title）
- 关联 task（如果存在）
- reviewer loop 状态
- fixer loop 状态
- 最近一次 run
- 当前阻塞摘要

---

## 3. UI 实现计划

### 3.1 MVP 页面

- Dashboard
- Loops 列表
- Task 详情
- PR / Run 详情
- Settings
- Config 查看

### 3.2 页面优先级

先只做只读页面，再补操作按钮。

### 3.3 关键组件

- Loop status badge
- Run timeline
- Checklist panel
- PR health summary
- Agent log viewer
- Config viewer

### 3.4 Agent 配置页建议

- MVP 阶段只做只读配置查看
- 多 profile / binding 页面后置到 Phase 2

---

## 4. 客户端 API 约束

- 所有状态修改都走 HTTP API
- CLI/UI 不缓存真相源
- 界面轮询或 SSE 均可，MVP 先轮询
