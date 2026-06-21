# Worker Loop 详细实现计划

## 1. 目标

根据用户提供的 spec 与 checklist，持续推进代码实现，直到创建 open PR。

---

## 2. 处理流水线

1. `prepare-task`
2. `prepare-worktree`
3. `plan-step`
4. `execute-step`
5. `validate-step`
6. `sync-checklist`
7. `open-pr`

---

## 3. 详细步骤

### 3.1 prepare-task

要求：

- task 必须绑定 spec
- 必须存在 checklist
- 必须确定 repo 与 base branch

### 3.2 prepare-worktree

- 创建 branch：`looper/task/{task-id}`
- 创建 worktree
- 记录 task -> worktree 映射

### 3.3 plan-step

每次只挑一到两个 checklist item 执行，避免 agent 一次做完整个任务。

### 3.4 execute-step

传入：

- 当前 checklist item
- spec 摘要
- 相关代码上下文
- 完成定义

### 3.5 validate-step

执行项目配置中的验证命令：

- lint
- test
- build

失败则把 checklist item 标记为 `blocked` 或保持 `doing`。

### 3.6 sync-checklist

- 完成的项标记 `done`
- agent 新增项标记 `source=agent`
- 重要新增项可要求人工确认

### 3.7 open-pr

- 生成 PR title/body
- 请求 reviewer
- 记录 PR 编号
- 发送通知

---

## 4. 幂等策略

- 使用 `taskId + checklistItemId + headSha? + taskType` 作为基础幂等键
- `execute-step` 前先检查该 checklist item 是否已有成功 run
- 已成功的 checklist item 不得被重复执行
- `open-pr` 前先检查 task 是否已存在关联 PR

---

## 5. 关键限制

- 不允许无限循环执行 agent
- 单次 run 必须有时间预算
- 单次 run 必须可恢复

### 5.1 失败阈值

- 建议增加 `maxConsecutiveFailures`
- 同一 checklist item 连续验证失败超过阈值后，将 task 标记为 `blocked`
- blocked 后只能人工恢复或显式重试

---

## 6. 失败处理

- worktree 创建失败：终止 task
- validate 失败：记录失败并等待下一次 fixer/worker 继续
- 创建 PR 失败：保留 worktree，允许重试

---

## 7. 接入共享 LoopRunner

Worker 通过 `LoopRunner<WorkerStep>` 执行，step handler 映射建议：

- `prepare-task` → `PrepareTaskStep`
- `prepare-worktree` → `PrepareWorktreeStep`
- `plan-step` → `PlanChecklistSliceStep`
- `execute-step` → `InvokeWorkerAgentStep`
- `validate-step` → `ValidateChecklistSliceStep`
- `sync-checklist` → `SyncChecklistStep`
- `open-pr` → `OpenPullRequestStep`

约束：

- 单次 run 只允许推进有限 checklist slice，不允许“直到做完为止”的无限 agent 回环
- `execute-step` 与 `open-pr` 都是有副作用 step，必须有独立 checkpoint
- `validate-step` 失败时可返回 `blocked` / `retryable` / `manual_intervention` 三类结果，交由 runner 统一处理

### 7.1 Worker 的迭代模型

建议采用**每个 checklist slice 一个 run**，而不是在单个 run 内部做循环跳转：

1. scheduler 为 task 入队
2. runner 执行一轮 `prepare -> plan -> execute -> validate -> sync`
3. 若 checklist 仍未完成，则重新入队下一轮 worker item
4. 全部完成后才进入 `open-pr`

这样 `LoopRunner` 可以保持线性 step sequencer，不需要引入 goto / cycle 语义。
