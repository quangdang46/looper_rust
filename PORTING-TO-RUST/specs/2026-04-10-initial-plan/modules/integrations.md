# 外部集成模块详细实现计划

## 1. 范围

- Coding Agent Adapter
- Agent Profile Resolver
- GitHub Gateway
- Git / Worktree Gateway
- Notification Gateway

---

## 2. Coding Agent Adapter

### 2.1 统一职责

- 检查 agent 是否可用
- 生成执行命令
- 传入 prompt/context
- 收集 stdout/stderr
- 结构化执行结果

### 2.2 统一输入

建议增加：

- `idempotencyKey`
- `budget`
- `env`
- `metadata`
- `timeoutMs`
- `maxOutputBytes`

同时建议引入：

- `workingDirectory`
- `cancellationToken`
- `expectedCompletionContract`
- `heartbeatTimeoutMs`
- `gracefulShutdownMs`

### 2.3 统一输出

- `status`
- `summary`
- `artifacts`
- `changedFiles`
- `commits`
- `rawLogs`
- `parseStatus`
- `completionSignal`
- `heartbeatCount`
- `resourceUsage`
- `pid`

### 2.3.1 统一执行生命周期

Looper 不应只抽象“调用 agent 并拿结果”，还应抽象整个运行期：

```ts
interface AgentExecution {
  pid: number
  startedAt: string
  status: 'running' | 'completed' | 'failed' | 'timeout' | 'killed'
  wait(): Promise<AgentResult>
  kill(reason: string): Promise<void>
}

interface AgentExecutor {
  start(input: AgentRunInput): Promise<AgentExecution>
}
```

标准生命周期：

1. spawn child process
2. 记录 pid / runId / loopId
3. 流式消费 stdout / stderr
4. 产生活跃 heartbeat
5. 检测 completion contract
6. 超时或取消时执行 `SIGTERM -> grace period -> SIGKILL`
7. 解析结果并落盘

### 2.4 执行约束

- `timeoutMs` 应视为必填，超时后 `SIGTERM -> 等待 5s -> SIGKILL`
- 必须限制 `stdout/stderr` 缓冲上限，避免长输出打满内存或磁盘
- 子进程应可被统一清理；`looperd stop` 时必须回收仍在运行的 agent 子进程
- 对写文件型 agent，必须绑定 worktree / cwd，不允许漂移到未知目录
- 必须记录 agent pid，供 `looperd` 崩溃恢复后查杀 orphan process
- 必须支持活跃心跳与 inactivity timeout，避免 agent 假死长期占坑

### 2.4.1 完成信号契约

参考 open-ralph-wiggum，建议 Looper 为 agent 输出定义统一 completion contract，而不是完全依赖自然语言总结。

可选方案：

- 结构化 JSON 结束块
- 明确 terminal marker
- 指定 artifact 文件落地

要求：

- `parse_failed` 时保留原始 completion payload
- 不允许在 `parse_failed` 后自动重放有副作用步骤
- 必须允许人工查看原始结果并手动恢复

### 2.5 解析失败降级策略

当 agent 未按预期返回结构化结果时：

- 保存 `rawLogs`
- 保存 `completion payload`
- run 标记为 `parse_failed`
- 不自动重放副作用步骤
- 只允许从解析检查点重试

### 2.6 Agent Registry

```ts
interface AgentRegistry {
  get(name: AgentName): CodingAgentAdapter
  listAvailable(): Promise<AgentName[]>
}
```

### 2.7 Agent Profile Resolver

真正执行前，Looper 应把项目配置、loop 绑定、临时覆盖解析成最终的运行参数。

```ts
interface ResolvedAgentProfile {
  vendor: AgentName
  model?: string
  params: Record<string, unknown>
  env: Record<string, string>
}
```

MVP 阶段直接读取 `config.md` 中的单一 `AgentConfig`；更复杂的 profile/binding 设计后置到 [`./agent-config.md`](./agent-config.md)。

### 2.8 预算与观测

agent 执行至少记录：

- 预估 token / cost
- wall time
- 输出字节数
- tool / command 次数（如果 agent 可观测）
- 最后一次 heartbeat 时间

这些数据既用于成本控制，也用于检测 stuck / struggling loops。

---

## 3. GitHub Gateway

### 3.1 能力清单

- 列出 open PR
- 获取 diff 与 review 状态
- 获取 comments / reviews / checks
- 创建 review / comment / approve
- 管理 reaction
- 创建 PR / request reviewers

### 3.2 数据归一化

必须把 gh 输出转换成内部模型：

- `PullRequestSnapshot`
- `ReviewState`
- `CheckState`
- `CommentThread`

---

## 4. Git / Worktree Gateway

### 4.1 能力

- 创建 branch
- 创建 / 恢复 / 删除 worktree
- 检查分支状态
- commit / push
- 检查冲突

### 4.2 安全约束

- 禁止直接操作受保护分支
- commit message 模板统一
- push 前校验当前分支绑定关系

### 4.3 worktree 生命周期

- task / PR 创建时分配 worktree
- PR merged / closed 后可进入待清理队列
- 清理前必须确认无 active loop 绑定
- 清理失败只记录告警，不影响主流程

---

## 5. Notification Gateway

MVP 至少支持：

- app 内通知
- macOS `osascript` 系统通知

### 5.1 第一阶段实现方式

第一期通知网关先实现：

- 应用内通知（持久化到系统内部）
- 本地系统通知：通过 `osascript -e 'display notification ...'`

适用前提：

- `looperd` 运行在 macOS 本机
- 用户希望接收本地桌面提醒

### 5.2 第二阶段扩展

第二阶段再补充：

- 飞书机器人 / 群消息
- 其他远程通知渠道

### 5.3 `osascript` 通知 payload 规范

建议先定义一层内部 payload，再由通知网关转换成 AppleScript 命令。

```ts
type SystemNotificationPayload = {
  id: string
  level: 'info' | 'warning' | 'action_required' | 'success' | 'failure'
  title: string
  subtitle?: string
  body: string
  sound?: string
  group?: 'reviewer' | 'worker' | 'fixer' | 'system'
  entityType?: 'loop' | 'run' | 'task' | 'pr' | 'project'
  entityId?: string
  dedupeKey?: string
  createdAt: string
}
```

字段约束建议：

- `title`：必填，建议不超过 60 字
- `subtitle`：可选，建议用于 repo / PR / task 标识
- `body`：必填，建议 1~2 句，避免过长
- `sound`：可选，默认关闭；`action_required / failure` 可开启
- `dedupeKey`：用于短时间内通知去重

### 5.4 AppleScript 映射建议

统一映射到：

```applescript
display notification "{body}" with title "{title}" subtitle "{subtitle}" sound name "{sound}"
```

实现约束：

- 必须做字符串转义，避免引号和换行破坏命令
- `subtitle` / `sound` 为空时不要拼接对应片段
- 不允许把未经清洗的 agent 原始输出直接作为 `body`

### 5.5 推荐通知文案模板

#### Reviewer

- `title`: `Reviewer 完成`
- `subtitle`: `<repo>#<pr-number>`
- `body`: `PR review 已完成：通过 / 需修改 / 执行失败`

#### Worker

- `title`: `Worker 进度更新`
- `subtitle`: `<task-id>`
- `body`: `Checklist 已完成 1 项` / `已创建 PR #123`

#### Fixer

- `title`: `Fixer 修复完成`
- `subtitle`: `<repo>#<pr-number>`
- `body`: `已提交修复并推送分支` / `本地验证失败，等待处理`

#### System

- `title`: `looperd 状态变更`
- `subtitle`: `system`
- `body`: `服务已启动` / `服务恢复完成` / `服务异常退出`

### 5.6 去重与节流建议

- 相同 `dedupeKey` 在短窗口内只发一次
- 默认节流窗口建议 30~60 秒
- 高频事件（如 run log 更新）不得直接发系统通知
- 只对状态跃迁事件发通知，不对普通轮询结果发通知

建议统一生成规则：

- `dedupeKey = {eventType}:{entityType}:{entityId}`

### 5.7 第一阶段必须通知的事件

- `looperd.started`
- `looperd.recovered`
- `reviewer.completed`
- `reviewer.failed`
- `worker.pr_opened`
- `worker.blocked`
- `fixer.pushed`
- `fixer.failed`
- `action.required`

### 5.8 审计与持久化

每次通知发送都应写入 `notifications` 和 `eventLogs`：

- payload 摘要
- 发送时间
- 发送结果（success / failed / skipped）
- 跳过原因（deduped / throttled / disabled）

### 5.9 事件级别建议

- info
- warning
- action_required
- success
- failure
