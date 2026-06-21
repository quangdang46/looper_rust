# Reviewer Loop 详细实现计划

## 1. 目标

自动发现、认领、Review、复查 open PR，并把结论同步回 GitHub 与通知系统。

---

## 2. 处理流水线

1. `discover`
2. `filter`
3. `claim`
4. `snapshot`
5. `review`
6. `publish`

---

## 3. 详细步骤

### 3.1 discover

来源：

- 我的 open PR
- 请求我 review 的 PR
- 带标签 PR

### 3.2 filter

过滤掉：

- 草稿 PR
- 已有活动锁的 PR
- 最近刚处理过且无新 commit 的 PR

### 3.3 claim

动作：

- 写 lock：`pr:{repo}:{pr}`
- 添加 👀 reaction
- 创建 run

### 3.4 snapshot

采集：

- 基础元数据
- diff
- checks
- unresolved comments
- reviews
- latest commit sha

### 3.5 review

生成统一 prompt，明确：

- review 范围
- 输出格式
- 是否允许 approve

### 3.6 publish

按结果写回：

- comment
- review
- approve
- reaction 更新
- 通知

### 3.7 watch

`watch` 不应成为长期占用资源的常驻步骤。

建议：

- watch 只表示“等待下一次 poll 再判断”
- 如果 head sha 变化，则由 scheduler 生成新的 review work item
- 如果 PR 已关闭或合并，则直接退出 review 生命周期

---

## 4. 幂等策略

- 使用 `repo + pr + headSha + taskType` 作为幂等 key
- 相同 head sha 不重复发评论
- publish 前检查历史记录

---

## 4.1 与其他 Loop 的竞争约束

- Reviewer 与 Fixer 不得同时持有同一 PR
- 当同一 PR 存在待处理 Reviewer run 时，Fixer 不应抢占
- PR 级锁优先于 loop 级锁

---

## 5. 失败处理

- agent 失败：保留 👀 但打失败标记，并计划重试
- publish 失败：重试写回，不重跑 review
- snapshot 失败：终止本次 run

---

## 6. 接入共享 LoopRunner

Reviewer 不单独实现自己的执行框架，而是把下列 step 映射给共享 `LoopRunner<ReviewerStep>`：

- `discover` → `DiscoverReviewerTargetsStep`
- `filter` → `FilterReviewerTargetsStep`
- `claim` → `ClaimReviewerTargetStep`
- `snapshot` → `SnapshotPullRequestStep`
- `review` → `InvokeReviewerAgentStep`
- `publish` → `PublishReviewResultStep`

约束：

- `review` 是唯一允许调用 agent 的 Reviewer step
- `publish` 必须可幂等重试，不得因 publish 失败而重新 review 同一 head sha
- `watch` 不作为常驻 step；由 scheduler 基于 head sha / review state 重新入队
