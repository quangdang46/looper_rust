# Fixer Loop 详细实现计划

## 1. 目标

自动修复 open PR 中的阻塞项，让 PR 回到可 review / 可 merge 状态。

---

## 2. 识别条件

满足任一条件即可入队：

- unresolved comments
- failing checks
- merge conflicts

---

## 3. 处理流水线

1. `discover-pr`
2. `claim-pr`
3. `collect-fixes`
4. `repair`
5. `validate`
6. `push`
7. `recheck`

claim 时应使用 PR 级锁：`pr:{repo}:{pr}`，避免与 Reviewer 并发修改同一 PR。

---

## 4. collect-fixes 设计

建议把问题归一化成：

```ts
type FixItem =
  | { type: 'comment'; id: string; summary: string }
  | { type: 'check'; name: string; summary: string }
  | { type: 'conflict'; files: string[] }
```

Agent 只消费 `FixItem[]`，不要直接消费混乱的原始 gh 输出。

---

## 5. validate 设计

- 跑必要测试
- 检查 git 状态是否干净
- 检查是否仍有冲突

---

## 5.1 幂等策略

- 使用 `prNumber + fixItemHash + headSha` 作为基础幂等键
- `repair` 前先检查同一 `FixItem` 是否已经成功修复
- 若 head sha 已变化，旧的 fix plan 自动失效并重新采集

---

## 6. 完成信号

通知“PR 已完成”前应满足：

- unresolved comments = 0
- failing checks = 0
- merge conflicts = 0
- review state 满足通过条件

建议强信号优先级：

1. GitHub Approve
2. 明确 review 通过 comment
3. reaction

---

## 7. 第一阶段（必须实现）

第一阶段的 Fixer Loop 不追求“完整闭环”，只要求具备最小可用修复能力：

### 7.1 范围

- 能发现满足条件的 open PR
- 能进入对应 worktree
- 能收集 `comment / failing check / conflict` 三类 FixItem
- 能调用单一 Agent 执行修复
- 能在修复后执行基础验证
- 能自动 commit + push
- 能把本次修复结果记录到 run / event log

### 7.2 第一阶段可暂缓项

- 自动判断“PR 已完成”并发送最终完成通知
- 更细粒度的 comment 线程状态同步
- 多轮 watch/recheck 长时间闭环
- 复杂冲突修复策略
- 多 Agent fallback

### 7.3 第一阶段验收标准

至少满足以下条件：

1. 当 open PR 存在 unresolved comments、失败 CI 或冲突时，可被 Fixer 识别并入队
2. Fixer 能恢复或进入正确 worktree
3. Agent 能拿到结构化 `FixItem[]` 输入
4. 修复后能执行项目定义的基础验证命令
5. 如果本地验证通过，Fixer 能提交并推送分支
6. 整个过程有可追踪的 run 记录和错误日志

---

## 8. 第二阶段补全项

第二阶段再补全以下能力：

- watch -> recheck 持续闭环
- PR 完成信号判定
- 自动完成通知与远程通知渠道广播
- 更稳定的 publish / retry / recovery 策略
- 更细的 FixItem 分类与策略化修复

---

## 9. 接入共享 LoopRunner

Fixer 通过 `LoopRunner<FixerStep>` 执行，step handler 映射建议：

- `discover-pr` → `DiscoverFixerTargetsStep`
- `claim-pr` → `ClaimFixerTargetStep`
- `collect-fixes` → `CollectFixItemsStep`
- `repair` → `InvokeFixerAgentStep`
- `validate` → `ValidateFixResultStep`
- `push` → `PushFixCommitStep`
- `recheck` → `RecheckPullRequestHealthStep`

约束：

- `repair` 是唯一允许调用 agent 的 Fixer step
- `collect-fixes` 必须输出稳定的 `FixItem[]` 快照，后续步骤不直接依赖原始 GitHub 输出
- `push` 与 `recheck` 必须分离，保证 push 成功但 recheck 失败时可以从后者恢复
