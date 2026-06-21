# Fixer Post-Repair File-by-File Plan

## 1. Core Fixer Flow

### `apps/looperd/src/fixer/index.ts`

- [ ] 更新 `FIXER_STEP_SEQUENCE`
  - [ ] 加入 `prepare-worktree`
  - [ ] 加入 `reconcile-commits`
  - [ ] 加入 `resolve-comments`
- [ ] 扩展 `FixItem`
  - [ ] comment 项增加 `threadId`
- [ ] 扩展 `FixerGitGateway` 接口
  - [ ] `createWorktree()`
  - [ ] `prepareWorktree()`
  - [ ] `inspectHead()`
  - [ ] `commit()`
  - [ ] 保留 `push()`
- [ ] 扩展 `FixerGitHubGateway` 接口
  - [ ] `resolveReviewThread()`
- [ ] 扩展 `FixerCheckpoint`
  - [ ] `worktree`
  - [ ] `reconcileCommits`
  - [ ] `resolvedComments`
  - [ ] 视需要细化 `repair` / `push` / `validation` 状态块
- [ ] 新增 `runPrepareWorktreeStep()`
  - [ ] 为 PR 创建或恢复 Fixer worktree
  - [ ] 记录 `headRefName` / `headSha` / `worktree.path`
- [ ] 修改 `runRepairStep()`
  - [ ] 使用 Fixer worktree 路径而非 `project.repoPath`
  - [ ] 加入“不要处理远端副作用”的 prompt 约束
  - [ ] 保留对 agent 自发 commit 的兼容
- [ ] 新增 `runReconcileCommitsStep()`
  - [ ] 检测新增 commits
  - [ ] 检测未提交改动
  - [ ] 必要时执行 looperd commit
  - [ ] 记录 `committedByAgent` / `committedByLooperd`
- [ ] 修改 `runValidateStep()`
  - [ ] 基于 worktree 执行
  - [ ] 支持一次额外 `reconcile -> validate` 收敛
- [ ] 修改 `runPushStep()`
  - [ ] 基于 worktree 执行
  - [ ] 远端 head 校验失败时返回可重试错误
- [ ] 新增 `runResolveCommentsStep()`
  - [ ] 仅处理本轮 `comment` fix items
  - [ ] resolve 已完成项按幂等成功处理
- [ ] 修改 `runRecheckStep()`
  - [ ] 保持最终状态校正
- [ ] 在 terminal state 加入 Fixer worktree cleanup 调度

### `apps/looperd/src/fixer/index.test.ts`

- [ ] 补充 `prepare-worktree` 行为测试
- [ ] 补充 worktree 路径传递给 agent 的测试
- [ ] 补充 agent 无 commit 时 looperd commit 的测试
- [ ] 补充 agent 已 commit 时的 reconcile 测试
- [ ] 补充 validate 产生新修改时的一次额外收敛测试
- [ ] 补充 push 前远端变化时安全退出测试
- [ ] 补充 `resolve-comments` 幂等测试
- [ ] 补充 terminal cleanup 测试

## 2. Git / Worktree Infra

### `apps/looperd/src/infra/git.ts`

- [ ] 复用现有 `createWorktree()` 能力给 Fixer 使用
- [ ] 增加 `prepareWorktree()`
  - [ ] fetch 远端分支
  - [ ] 校验 `expectedHeadSha`
  - [ ] 必要时 reset 到 `origin/<branch>`
  - [ ] 检查 worktree 是否干净
- [ ] 增加 `inspectHead()`
  - [ ] 返回当前 `headSha`
  - [ ] 返回 `newCommitShas`
  - [ ] 返回 `hasUncommittedChanges`
  - [ ] 返回 `changedFiles`
- [ ] 增加 `commit()`
  - [ ] 只提交当前 worktree 改动
  - [ ] 不改 git config
- [ ] 如有需要，为 push 增加远端 head 预检查辅助能力
- [ ] 确保 `cleanupWorktree()` 可直接复用于 Fixer

### `apps/looperd/src/infra/git.test.ts`

- [ ] 为 `prepareWorktree()` 增加测试
- [ ] 为 `inspectHead()` 增加测试
- [ ] 为 `commit()` 增加测试
- [ ] 增加 Fixer worktree 复用 + reset 测试
- [ ] 继续确保 `push()` 不会 force push

### `apps/looperd/src/infra/index.ts`

- [ ] 如当前有统一导出层，补导出新增 git/github gateway 能力

## 3. GitHub Infra

### `apps/looperd/src/infra/github.ts`

- [ ] 扩展 `GitHubPullRequestDetail` 所需字段，支持稳定拿到 comment 对应 `threadId`
- [ ] 选择实现路径：
  - [ ] 扩展 `viewPullRequest()` 查询 thread 级信息
  - [ ] 或新增 thread 查询 helper / API 调用
- [ ] 增加 `resolveReviewThread()`
  - [ ] 基于 `gh api` / GraphQL 执行 resolve
  - [ ] 已 resolve 情况按幂等成功处理

### `apps/looperd/src/infra/github.test.ts`

- [ ] 为 threadId 数据解析增加测试
- [ ] 为 `resolveReviewThread()` 成功路径增加测试
- [ ] 为“already resolved”路径增加测试
- [ ] 为错误路径（不存在 / 权限不足）增加测试

## 4. Domain / Shared Types

### `apps/looperd/src/domain/index.ts`

- [ ] 更新 `FIXER_STEPS`
  - [ ] `prepare-worktree`
  - [ ] `reconcile-commits`
  - [ ] `resolve-comments`
- [ ] 如需要，补充新的 audit event type 常量

### `apps/looperd/src/storage/types.ts`

- [ ] 视当前持久化边界决定是否扩展：
  - [ ] `WorktreeRecord` 是否需要额外 Fixer 元数据
  - [ ] 事件 / 记录结构是否需要承载新 step 结果
- [ ] 确认 `QueueFailureKind` 现有枚举是否足够表达新的可重试/人工介入语义

### `apps/looperd/src/storage/sqlite/sqlite-store.ts`

- [ ] 确认 run checkpoint JSON 持久化无需额外 schema 变更
- [ ] 若扩展了 worktree/event 持久化字段，更新 store 映射

### `apps/looperd/src/storage/sqlite/migrate.ts`

- [ ] 仅当 SQLite schema 需要新列/表时修改

### `apps/looperd/src/storage/sqlite/migrations/*`

- [ ] 仅当 SQLite schema 需要新列/表时新增 migration

## 5. Runtime / Wiring

### `apps/looperd/src/runtime/index.ts`

- [ ] 确认 `FixerLoopRunner` 注入的是具备新能力的 git/github gateway
- [ ] 如需 terminal cleanup hook 或新配置项，在此完成 wiring

## 6. Prompt / Agent Behavior

### `apps/looperd/src/infra/agent-prompt.ts`

- [ ] 无需大改；只确认 Fixer 新 prompt 约束与 completion marker 共存正常

### `apps/looperd/src/fixer/index.ts`（prompt builder 部分）

- [ ] 在 `buildFixerPrompt()` 中补一句：
  - [ ] 专注代码修改
  - [ ] 避免 push / resolve remote review state
  - [ ] commit 尽量避免，但兼容 agent 已 commit

## 7. Likely No-Change / Optional Files

### `apps/looperd/src/infra/agent.ts`

- [ ] 预期无需为本方案做主改动
- [ ] 仅在后续决定统一注入 git author metadata 时再改

### `apps/looperd/src/infra/agent.test.ts`

- [ ] 本轮通常无需修改，除非顺带补 agent env / commit metadata 行为

## 8. Recommended Implementation Order

1. [ ] `domain/index.ts`
2. [ ] `fixer/index.ts`（类型、step 序列、checkpoint）
3. [ ] `infra/git.ts`
4. [ ] `infra/github.ts`
5. [ ] `fixer/index.ts`（具体 step 实现）
6. [ ] `fixer/index.test.ts`
7. [ ] `infra/git.test.ts`
8. [ ] `infra/github.test.ts`
9. [ ] `runtime/index.ts`
10. [ ] cleanup / audit events / optional storage follow-up
