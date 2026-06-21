# Fixer Post-Repair Implementation Checklist

## Phase 1: Data Model & Contracts

- [x] 更新 `FIXER_STEP_SEQUENCE`
  - [x] 增加 `prepare-worktree`
  - [x] 增加 `reconcile-commits`
  - [x] 增加 `resolve-comments`
  - [x] 确认 step 顺序为 `discover-pr -> claim-pr -> collect-fixes -> prepare-worktree -> repair -> reconcile-commits -> validate -> push -> resolve-comments -> recheck`

- [x] 扩展 `FixerCheckpoint`
  - [x] 增加 `worktree` 状态块
  - [x] 增加 `reconcileCommits` 状态块
  - [x] 增加 `resolvedComments` 状态块
  - [x] 明确每个 step 的 completed / retryable 判定字段

- [x] 扩展 `FixItem`
  - [x] comment 类型增加 `threadId`
  - [x] 明确旧 `id` 与 `threadId` 的职责边界

## Phase 2: Gateway / Infra

- [x] 扩展 `FixerGitGateway`
  - [x] 增加 `createWorktree()`
  - [x] 增加 `prepareWorktree()`
  - [x] 增加 `inspectHead()`
  - [x] 增加 `commit()`
  - [x] 复核 `push()` 不会 force push

- [x] 扩展 `FixerGitHubGateway`
  - [x] 增加 `resolveReviewThread()`
  - [x] 明确 thread 已 resolve 时的幂等语义

- [x] 获取 comment 对应的 `threadId`
  - [x] 选择实现路径：扩展 `viewPullRequest()` 或追加 thread 查询
  - [x] 确保 `collect-fixes` 输出稳定 thread 标识

## Phase 3: Fixer Workflow

- [x] 实现 `prepare-worktree` step
  - [x] 为 PR 创建或恢复 Fixer 专属 worktree
  - [x] fetch 目标 branch
  - [x] 校验预期 `headSha`
  - [x] 必要时 reset 到 `origin/<headRefName>`
  - [x] 确保进入 `repair` 前 worktree 干净
  - [x] 将 worktree 信息写入 checkpoint

- [x] 调整 `repair` step
  - [x] agent 只在 Fixer worktree 中运行
  - [x] 不再直接使用 `project.repoPath`
  - [x] prompt 增加“不要处理远端副作用”的说明
  - [x] 兼容 agent 自发 commit

- [x] 实现 `reconcile-commits` step
  - [x] 识别本轮新增 commit SHA
  - [x] 检查是否存在未提交修改
  - [x] 若未提交且允许 auto-commit，则由 looperd commit
  - [x] 若未提交且不允许 auto-commit，则明确失败
  - [x] 记录 `committedByAgent` / `committedByLooperd`
  - [x] 第一阶段不做 squash，多 commit 直接记录

- [x] 调整 `validate` step
  - [x] 在 `reconcile-commits` 之后执行
  - [x] 保持验证命令可配置
  - [x] 若验证产生新修改，允许一次额外的 `reconcile-commits -> validate` 收敛
  - [x] 若仍持续产出修改，则失败

- [x] 调整 `push` step
  - [x] 只从 Fixer worktree push
  - [x] push 前比较远端 head 是否变化
  - [x] 远端已变化时，以可重试失败结束
  - [x] 不做 force push

- [x] 实现 `resolve-comments` step
  - [x] 仅处理本轮 `FixItem.type === 'comment'`
  - [x] 只在 validate + push 成功后执行
  - [x] 已 resolve thread 视为幂等成功
  - [x] 逐项记录 resolve 结果到 checkpoint

- [x] 调整 `recheck` step
  - [x] 将其作为最终状态校正步骤
  - [x] 验证 unresolved comments / failing checks / conflicts 最新状态

## Phase 4: Recovery, Cleanup, Observability

- [x] 补充重试语义
  - [x] `prepare-worktree` head 变化 -> 可重试失败
  - [x] `push` 前远端变化 -> 可重试失败
  - [x] worktree 脏且来源不明 -> 人工介入

- [x] 增加事件记录
  - [x] worktree prepared
  - [x] commits reconciled
  - [x] comments resolved
  - [x] push skipped / retried / conflicted

- [x] 增加 worktree cleanup
  - [x] loop terminal state 时清理 Fixer worktree
  - [x] cleanup 失败时记录事件，不吞错误上下文

## Phase 5: Tests

- [x] Git gateway 测试
  - [x] create/restore worktree
  - [x] prepareWorktree reset 行为
  - [x] inspectHead 正确识别 commits / dirty tree
  - [x] commit 行为不污染全局 git config

- [x] GitHub gateway 测试
  - [x] resolveReviewThread 成功
  - [x] 已 resolve thread 幂等成功
  - [x] thread 不存在 / 权限问题错误处理

- [x] Fixer loop 单元/集成测试
  - [x] 使用独立 worktree 执行 repair
  - [x] agent 无 commit 时 looperd 自动 commit
  - [x] agent 已 commit 时正确识别并继续
  - [x] validate 产生新修改时的一次额外收敛
  - [x] push 前远端 head 变化时安全退出
  - [x] resolve-comments 只在 push 成功后执行
  - [x] terminal state 后 worktree cleanup

## MVP Cut Line

MVP 必须完成：

- [x] Fixer worktree 隔离
- [x] `prepare-worktree`
- [x] `reconcile-commits`
- [x] `resolve-comments`
- [x] `threadId` 数据链路
- [x] push 前远端 head 校验
- [x] terminal state cleanup

后续再做：

- [ ] TTL worktree reaper
- [ ] 更复杂的 commit message/trailer
- [ ] comment resolve 批量优化
- [ ] agent 多 commit 整理 / squash 策略
- [ ] 基于 changedFiles 的更细粒度通知
