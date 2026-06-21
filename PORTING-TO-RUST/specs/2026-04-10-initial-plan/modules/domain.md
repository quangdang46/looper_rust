# 核心领域模型详细实现计划

## 1. 目标

用稳定的领域模型承载三类 loop 的状态推进，避免业务逻辑散落在 controller 和 shell 调用里。

---

## 2. 建议聚合

- `Project`
- `Loop`
- `Run`
- `Task`
- `TaskItem`
- `PullRequestSnapshot`
- `Lock`

---

## 2.1 共享执行抽象

为了避免 Reviewer / Worker / Fixer 长期各自复制一套状态推进逻辑，领域层**最终可收敛到**如下共享抽象。

但这不是 MVP 前置条件：第一阶段允许各 loop 先用线性 `async` 流程实现，只要状态、锁和恢复语义保持一致即可。

```ts
interface LoopRunner<TStep extends string, TContext> {
  run(input: RunLoopInput<TStep, TContext>): Promise<RunLoopResult>
  cancel(input: { loopId: string; reason: string }): Promise<void>
  resume(input: { loopId: string }): Promise<RunLoopResult>
}

interface StepHandler<TStep extends string, TContext, TResult = unknown> {
  step: TStep
  execute(ctx: StepExecutionContext<TContext>): Promise<StepExecutionResult<TResult>>
}

type RunLoopInput<TStep extends string, TContext> = {
  loopId: string
  runId: string
  currentStep: TStep
  context: TContext
  checkpoint?: LoopCheckpoint<TStep>
}

type RunLoopResult = {
  status: 'success' | 'failed' | 'cancelled' | 'interrupted' | 'parse_failed'
  lastCompletedStep?: string
  nextStep?: string
  summary?: string
}

type LoopCheckpoint<TStep extends string> = {
  step: TStep
  persistedAt: string
  payload: Record<string, unknown>
}
```

若未来提炼共享 runner，边界约束应为：

- `LoopRunner` 负责 step sequencing、checkpoint、超时、重试、取消、恢复
- `StepHandler` 只负责某一步的业务逻辑
- 领域状态迁移必须由 runner 驱动，不允许散落到 service / controller 分支中

---

## 3. 关键规则

### 3.1 Loop

- `Loop` 存的是一个持续自动化流程实例，不是任务本身
- 每个 `Loop` 都必须绑定一个明确 target
- 同一 `project + type + target` 只能有一个 active loop
- `paused` loop 不能被 scheduler 自动执行

建议显式建模：

```ts
type LoopTarget =
  | { targetType: 'task'; taskId: string }
  | { targetType: 'pull_request'; repo: string; prNumber: number }
```

补充约束：

- Worker loop 通常绑定 `task`
- Reviewer loop 绑定 `pull_request`
- Fixer loop 绑定 `pull_request`
- 同一个 task 在 MVP 阶段只对应一个 worker loop
- Reviewer / Fixer 对同一 PR 仍然是一对一执行单元

### 3.2 Run

- 每个 run 必须绑定一个 loop
- `running` -> 结束态只能发生一次

### 3.3 Task / TaskItem / Pull Request

- Worker 只能在存在 checklist 时开始
- Agent 补充的 checklist item 必须标记 `source=agent`
- PR 创建策略默认是“所有 checklist 完成前不开 PR”，但应允许配置覆盖
- 一个 `Task` 可以尚未关联任何 PR
- MVP 阶段一个 `Task` 最多关联一个 PR
- MVP 阶段一个 Worker 只负责一个 Task 对应的 PR

MVP 建议直接把 PR 关联放在 `tasks` 上：

```ts
type Task = {
  id: string
  projectId: string
  title: string
  specPath?: string
  prNumber?: number
  status: string
}
```

MVP 约束：

- 一个 task 最多关联一个 PR
- 一个 PR 最多关联一个 task
- Worker 创建 PR 后，后续 review / fix 流程围绕该 PR 自动串联
- Reviewer / Fixer 不需要用户为常规 task 流程手动单独启动

### 3.4 Lock

- 锁必须有 owner、key、expiresAt
- 锁过期后允许抢占

---

## 4. 值对象建议

- `LoopType`
- `LoopStatus`
- `RunStatus`
- `LoopStep`
- `ReviewConclusion`
- `PRHealth`
- `NotificationLevel`

补充一个关键快照对象：

```ts
type PullRequestSnapshot = {
  id: string
  projectId: string
  repo: string
  prNumber: number
  headSha: string
  baseSha?: string
  title: string
  body?: string
  author: string
  diffRef?: string
  checksSummary?: string
  unresolvedThreadCount: number
  reviewState?: string
  capturedAt: string
}
```

---

## 5. 状态机落地

## 5.1 通用状态枚举

```ts
type LoopStatus =
  | 'idle'
  | 'queued'
  | 'running'
  | 'paused'
  | 'completed'
  | 'failed'
  | 'interrupted'

type RunStatus =
  | 'queued'
  | 'running'
  | 'success'
  | 'failed'
  | 'cancelled'
  | 'interrupted'
  | 'parse_failed'
```

### 合法迁移

- `LoopStatus`: `idle -> queued -> running -> completed|failed|paused|interrupted`
- `paused -> queued|completed`
- `interrupted -> queued|failed`
- `RunStatus`: `queued -> running -> success|failed|cancelled|interrupted|parse_failed`

## 5.2 三类 Loop 的 step 枚举

```ts
type ReviewerStep = 'discover' | 'filter' | 'claim' | 'snapshot' | 'review' | 'publish'
type WorkerStep = 'prepare-task' | 'prepare-worktree' | 'plan-step' | 'execute-step' | 'validate-step' | 'sync-checklist' | 'open-pr'
type FixerStep = 'discover-pr' | 'claim-pr' | 'collect-fixes' | 'repair' | 'validate' | 'push' | 'recheck'
```

说明：

- `watch` 不作为长期占用的运行态 step
- watch 行为由下一轮 scheduler poll 自然覆盖
- `interrupted` 必须记录最后成功 step，供恢复时重入

## 5.3 可恢复性标注

- Reviewer：`snapshot / review / publish` 可恢复
- Worker：`plan-step / execute-step / validate-step / open-pr` 可恢复
- Fixer：`collect-fixes / repair / validate / push` 可恢复

恢复时规则：

- 若最后一步未产生外部副作用，则从该 step 重试
- 若最后一步已产生外部副作用，则从下一步或幂等检查点继续

进一步要求：恢复策略应成为 step 元数据的一部分，而不是靠调用方猜测。

```ts
type ResumePolicy = 'replay_step' | 'advance_from_checkpoint' | 'manual_intervention'
```

建议每个 Loop 单独实现 transition 函数：

```ts
function transitionReviewerLoop(state: ReviewerLoopState, event: ReviewerEvent) {
  // ...
}
```

不要把状态迁移藏在 service 逻辑分支里。

---

## 5.4 Step 执行契约（目标形态）

如果后续收敛到统一 runner，可采用统一阶段：

1. `guard`：检查锁、幂等 key、输入快照、取消信号
2. `execute`：执行外部副作用（agent / git / github / notify）
3. `persist`：把 step 结果、checkpoint、下一状态写入事务
4. `emit`：异步发出事件给 HookBus

目标约束：

- 只有 `persist` 能改变 loop / run 的持久化状态
- 若进程在 `execute` 后崩溃，恢复逻辑必须依赖幂等 key 或 checkpoint 决定重放/跳过
- 每个 step 都必须显式声明 `timeoutMs`、`resumePolicy`

```ts
interface StepDefinition<TStep extends string> {
  step: TStep
  timeoutMs: number
  resumePolicy: ResumePolicy
}
```

MVP 阶段不要求每个 step 都机械拆成 guard/execute/persist/emit 四段；只要求：副作用、状态落盘、错误处理边界清楚。

---

## 5.5 取消与抢占

`paused` / `cancelled` 不能只停留在状态字段，必须可传递到执行链路：

- scheduler 停止继续派发该 loop 的新 queue item
- runner 设置 cancellation token
- step handler 在 guard 与长执行点检查取消信号
- agent 执行器收到取消后执行 `SIGTERM -> grace period -> SIGKILL`

若未来抽出统一 runner，建议统一使用：

```ts
interface LoopCancellationToken {
  readonly cancelled: boolean
  readonly reason?: string
}
```

### 5.5.1 Checkpoint 落地

目标上可把 checkpoint 作为 `runs` 表上的结构化字段持久化：

- `current_step`
- `last_completed_step`
- `checkpoint_json`
- `last_heartbeat_at`

但 MVP 阶段可先采用更简单的恢复语义：记录最后成功 step，从下一步继续。

---

## 6. 审计事件

建议内建这些事件：

- `loop.created`
- `loop.started`
- `loop.step.started`
- `loop.step.completed`
- `loop.step.failed`
- `run.started`
- `run.cancelled`
- `agent.invoked`
- `agent.heartbeat`
- `agent.completed`
- `agent.timed_out`
- `agent.killed`
- `pr.review.posted`
- `task.checklist.updated`
- `notification.sent`
