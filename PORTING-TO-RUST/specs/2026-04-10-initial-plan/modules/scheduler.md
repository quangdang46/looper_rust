# Scheduler / Queue 详细实现计划

## 1. 目标

把“发现工作”和“执行工作”分离，避免 loop 逻辑直接写成无限轮询脚本。

---

## 2. 模块职责

- 周期扫描
- work item 入队
- 执行限流
- 失败重试
- 超时控制
- 锁协调

---

## 3. 数据结构

```ts
type QueueItem = {
  id: string
  type: 'reviewer' | 'worker' | 'fixer'
  targetId: string
  dedupeKey: string
  scheduledAt: string
  attempts: number
}
```

`QueueItem` 不应仅存在内存里，必须持久化到 storage，以便 looperd 重启后恢复待执行工作。

---

## 4. 执行模型

### 4.1 scanner

负责按固定周期发现候选 PR / task。

### 4.2 planner

负责把候选对象转成 `QueueItem`。

### 4.2.1 Loop 优先级

建议优先级：

1. `reviewer`
2. `fixer`
3. `worker`

原因：

- 先完成 review 决策，再进入 fixer 更安全
- worker 通常周期更长，优先级可低于面向已有 PR 的闭环任务

### 4.3 executor

负责消费 queue item、创建/恢复 run，并把执行委托给统一的 `LoopRunner`。

建议职责切分：

1. 取出 `QueueItem`
2. 获取业务锁
3. 创建或恢复 `Run`
4. 调用 `LoopRunner.run()`
5. 根据结果更新 queue item、重试时间、loop 状态

即：scheduler 负责 `when`，runner 负责 `how`。

### 4.4 锁模型

MVP 单进程下先不引入 lease，只保留业务锁：

- `pr:{repo}:{pr}`
- `task:{taskId}`

如果未来出现多 executor / 多进程竞争，再补 queue lease。

### 4.5 优先级老化（后置）

priority aging 先记为后续优化，不进入 MVP：

- 初始优先级：`reviewer > fixer > worker`

---

## 5. 重试策略

- 默认最多 3 次
- 指数退避
- 区分可重试与不可重试错误

补充分类：

- `retryable_transient`：网络波动、GitHub 暂时失败、agent timeout
- `retryable_after_resume`：中断恢复、checkpoint 可安全续跑
- `non_retryable`：输入非法、配置缺失、受保护分支违规
- `manual_intervention`：parse_failed、预算耗尽、重复副作用无法确认

---

## 6. 并发策略

- 同一 PR / task 同时只能有一个 active item
- Reviewer / Worker / Fixer 可以分别限流

补充约束：

- 同一 PR 使用统一锁 `pr:{repo}:{pr}`
- 同一 task 使用统一锁 `task:{taskId}`
- 当 reviewer item 存在时，fixer item 不得抢占同一 PR

### 6.1 取消与恢复

- `pause`/`cancel` 会先标记 loop 状态，再通知 runner 取消当前执行
- executor 崩溃恢复后，先校验 lock / run checkpoint，再决定 resume 或重新入队
