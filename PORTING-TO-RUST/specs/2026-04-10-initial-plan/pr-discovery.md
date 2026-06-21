# Reviewer / Fixer PR 自动发现机制

本文档补充说明：Reviewer 和 Fixer 是如何自动发现 PR、何时创建或唤起 loop、以及如何避免重复执行的。

---

## 1. 目标

需要解决 4 个问题：

1. Reviewer 怎么自动发现候选 PR
2. Fixer 怎么自动发现候选 PR
3. 什么时候创建 loop，什么时候只创建新的 run / queue item
4. 如何避免重复发现、重复入队、重复副作用

---

## 2. 总体机制

统一采用：

**scheduler scanner 周期扫描 → planner 归一化候选对象 → enqueue queue item → executor 取出执行**

MVP 阶段不依赖 webhook，先用轮询。

### 2.1 scanner 的两个扫描器

- `reviewerScanner`
- `fixerScanner`

它们都按固定周期运行，但使用不同判定规则。

### 2.2 scanner 输出

scanner 不直接创建 run，只输出候选对象：

```ts
type PullRequestCandidate = {
  projectId: string
  repo: string
  prNumber: number
  headSha: string
  author: string
  isDraft: boolean
  reviewState?: string
  unresolvedCommentCount?: number
  failingCheckCount?: number
  hasConflicts?: boolean
}
```

planner 再把它转成 queue item。

---

## 3. Reviewer 自动发现

## 3.1 候选来源

MVP 只支持这几类来源：

1. 当前项目中的 open PR
2. 由当前用户创建的 open PR
3. 明确请求 review 的 open PR

实现上优先使用 `gh pr list` / `gh pr view` 组合获取。

### 3.2 Reviewer 候选过滤

候选 PR 满足以下条件才继续：

- PR 处于 open
- 不是 draft
- 不存在同一 PR 的 active reviewer lock
- 最近没有针对同一 `headSha` 成功 publish reviewer 结果

补充：

- 如果 PR 是 worker 刚创建/更新的，也应进入 reviewer 候选集
- 如果 `headSha` 未变化，默认不重复 review

### 3.3 Reviewer 入队条件

满足任一条件即可 enqueue reviewer item：

1. 从未被 reviewer 处理过
2. `headSha` 相比上次成功 review 已变化
3. 上次 reviewer run 失败且允许重试
4. 用户显式触发 `looper loop start --type reviewer --pr ...`

### 3.4 Reviewer queue item

```ts
type ReviewerQueueItem = {
  type: 'reviewer'
  targetId: string
  repo: string
  prNumber: number
  dedupeKey: string // repo + pr + headSha + reviewer
}
```

`dedupeKey` 推荐：

```txt
reviewer:{repo}:{prNumber}:{headSha}
```

---

## 4. Fixer 自动发现

## 4.1 候选来源

MVP 也从 open PR 扫描开始，但只挑存在阻塞项的 PR。

### 4.2 Fixer 阻塞项判定

满足任一条件即可进入 fixer 候选集：

1. unresolved review comments > 0
2. failing checks > 0
3. merge conflict = true

MVP 建议：

- unresolved comments：看 review thread / unresolved review comments
- failing checks：看 PR 当前失败的 checks
- merge conflict：看 GitHub PR mergeability 或 git 检查结果

### 4.3 Fixer 过滤条件

候选 PR 还需满足：

- PR 处于 open
- 不存在 active reviewer lock
- 不存在 active fixer lock
- 当前 `headSha` 下这批 fix items 还没成功修过

### 4.4 Fixer 入队条件

满足任一条件即可 enqueue fixer item：

1. 首次发现阻塞项
2. 上次修复后仍存在阻塞项且 `headSha` 已变化
3. 上次 fixer run 失败且允许重试
4. 用户显式触发 `looper loop start --type fixer --pr ...`

### 4.5 Fixer queue item

```ts
type FixerQueueItem = {
  type: 'fixer'
  targetId: string
  repo: string
  prNumber: number
  dedupeKey: string // repo + pr + headSha + fix-signal-hash
}
```

推荐：

```txt
fixer:{repo}:{prNumber}:{headSha}:{fixSignalHash}
```

其中 `fixSignalHash` 由以下信息归一化后生成：

- unresolved comment ids
- failing check names
- conflict marker

---

## 5. 何时创建 Loop，何时只创建新的 Run

这是最关键的实现规则之一。

## 5.1 基本原则

- **Loop 是长期执行实体**
- **Run 是某次执行记录**
- **Queue item 是一次待执行机会**

### 5.2 Reviewer

对某个 PR：

- 如果还没有 reviewer loop：创建 reviewer loop
- 如果 reviewer loop 已存在但当前 idle / queued / interrupted：复用同一个 loop，创建新的 run
- 如果 reviewer loop 已在 running：不重复创建 loop，也不重复入队同一 `dedupeKey`

### 5.3 Fixer

对某个 PR：

- 如果还没有 fixer loop：创建 fixer loop
- 如果 fixer loop 已存在但当前 idle / queued / interrupted：复用同一个 loop，创建新的 run
- 如果 fixer loop 已在 running：不重复创建 loop，也不重复入队同一 `dedupeKey`

### 5.4 Worker 自动串联 Reviewer / Fixer

正常 task 流程中：

1. worker 创建 PR
2. scheduler 在下一轮扫描中发现这个 PR 符合 reviewer 条件
3. 自动创建或唤起 reviewer loop
4. 之后如果 PR 出现阻塞项
5. scheduler 自动创建或唤起 fixer loop

也就是说：

- worker 不直接执行 reviewer/fixer 的业务逻辑
- worker 只负责让 PR 进入可被 scanner 发现的状态
- reviewer / fixer 的实际创建/唤起仍由 scheduler 统一负责

---

## 6. 如何避免重复发现与重复执行

## 6.1 扫描去重

同一轮 scanner 中：

- 同一个 `repo + prNumber` 只保留一个候选对象

## 6.2 planner 去重

planner 在 enqueue 前检查：

- 是否已有同 `dedupeKey` 的 queue item 未完成
- 是否已有同 `dedupeKey` 的成功 run

有则跳过，不再重复入队。

## 6.3 executor 去重

executor 在真正执行前再次检查：

- PR 锁是否已被占用
- 当前 `headSha` 是否仍匹配 queue item
- 对应副作用是否已成功落地

若条件不再成立，则直接丢弃该 queue item 或标记为 skipped。

## 6.4 publish / push 去重

- reviewer publish 以 `repo + pr + headSha` 作为核心幂等边界
- fixer push 以 `repo + pr + headSha + fixSignalHash` 作为核心幂等边界

---

## 7. 推荐轮询节奏

MVP 建议：

- reviewer scanner：10~30 秒
- fixer scanner：10~30 秒
- worker 调度：可略慢，但不应过长

原则：

- reviewer / fixer 面向已有 PR 闭环，应比 worker 优先级更高
- 先保证已有 PR 的反馈闭环，再推进新的开发工作

---

## 8. 用户可感知行为

从用户视角，应该看到的是：

### 8.1 正常 task 驱动流程

1. 用户启动 task
2. worker 推进并创建 PR
3. reviewer 自动出现
4. 如果 PR 有阻塞项，fixer 自动出现

用户不需要手动理解 scanner / planner / queue。

### 8.2 已有 PR 托管流程

1. 用户已有 PR
2. 用户手动启动 reviewer 或 fixer
3. 之后系统继续自动发现并持续处理该 PR

---

## 9. MVP 明确不做的事

当前阶段先不做：

- webhook 驱动发现
- CODEOWNERS 智能 review 路由
- 多项目跨仓统一扫描协调
- 基于复杂优先级策略的 PR 智能抢占
- reviewer / fixer 自动创建独立 task

---

## 10. 一句话总结

MVP 的自动发现机制是：

**scheduler 定期扫描 open PR，按 reviewer / fixer 各自规则筛选候选对象，用 `dedupeKey` 防重后入队，再由 executor 在锁保护下创建或唤起对应 loop 并执行。**
