# Looper 初始方案

## 1. 项目目标

Looper 是一个面向研发流程自动化的工具，目标是将 PR review、任务开发、PR 修复这三类重复工作交给后台常驻服务持续执行，并通过 CLI / UI 提供统一入口。

一句话定义：**Looper = 面向 PR 与任务流的常驻式 Coding Agent 调度系统**。

---

## 2. 设计原则

1. **Agent 无关**：统一抽象 claude code、codex、opencode、cursor cli。
2. **服务端中心化**：状态、调度、审计统一收敛到 looperd。
3. **GitHub 优先**：以 PR、Review、Comment、CI 为主要事件源。
4. **可恢复 + 幂等**：任何 loop 中断后可恢复，重复触发不产生重复副作用。
5. **Spec 驱动**：大功能先做 Spec PR，再进入 Worker/Fixer 流程。

---

## 3. 技术栈

- Runtime：**Bun**
- Repo：**Monorepo**
- Language：**TypeScript**
- Service：`looperd`
- Client：CLI、Web UI
- External：GitHub CLI、Git、本地 Coding Agent CLI、macOS `osascript` 系统通知

---

## 4. 推荐 Monorepo 结构

MVP 不建议一开始拆太多 package。

第一阶段推荐先把核心实现都放在 `apps/looperd/src/` 下，按目录分层：

```txt
apps/
  looperd/
    src/
      bootstrap/
      server/
      app/
      domain/
      infra/
      storage/
      runtime/
  cli/
  web/

specs/
```

只有当某个模块被第二个消费者复用时，再提取到 `packages/`。

后续可演进结构：

```txt
apps/
  looperd/
  cli/
  web/

packages/
  core/
  loop-runner/
  agent-adapters/
  github/
  git/
  scheduler/
  hooks/
  notifications/
  config/
  logger/

specs/
```

---

## 5. 核心模块摘要

### 5.1 looperd

后台常驻服务，负责生命周期、HTTP API、调度、恢复、持久化与外部系统编排。

- 详细实现计划：[`./modules/looperd.md`](./modules/looperd.md)
- 配置 schema：[`./modules/config.md`](./modules/config.md)

补充说明：在 macOS 上，`looperd` 长期推荐通过 `launchd` 托管；但开发期仍保留 foreground 模式。

补充说明：spec 已区分**开发阶段运行流程**与**npm 分发阶段运行流程**，分别用于本地调试与面向用户的自动升级/daemon 运行。

### 5.2 CLI / UI

CLI / UI 均为 `looperd` 客户端，不直接操作 GitHub、Git、Agent。

- 详细实现计划：[`./modules/clients.md`](./modules/clients.md)

### 5.3 外部集成

统一抽象 Coding Agent、GitHub、Git/worktree、通知通道，避免业务逻辑散落在 shell 命令中。

- 详细实现计划：[`./modules/integrations.md`](./modules/integrations.md)
- Phase 2 Agent 配置预研：[`./modules/agent-config.md`](./modules/agent-config.md)

### 5.4 核心领域模型

围绕 `Project / Loop / Run / Task / Checklist / Lock` 构建稳定状态机与审计事件。

- 详细实现计划：[`./modules/domain.md`](./modules/domain.md)
- 存储接口设计：[`./storage.md`](./storage.md)

### 5.5 Loop Runner

`LoopRunner<TStep>` 是**目标架构**，不是 MVP 前置条件。

理想上它会作为三类 loop 的共享执行底座，负责：

- step 编排与推进
- checkpoint 持久化
- interruption / resume
- timeout / retry / cancellation
- 统一事件发射

但第一条链路实现时，允许先在具体 loop 内直接写线性执行逻辑；待 Reviewer / Worker / Fixer 都跑通后，再决定是否提炼成统一 runner。

- 详细实现计划：[`./modules/domain.md`](./modules/domain.md)、[`./modules/scheduler.md`](./modules/scheduler.md)

### 5.6 Hooks / Extension Bus

HookBus 也是**目标架构**，不是 MVP 前置条件。

它用于把通知、审计、metrics、trace、调试输出等横切能力从核心 loop 中抽离。

补充：MVP 阶段可以不实现 HookBus，直接在关键路径写 event log / notification；若实现，也只做静态注册少量内建 hook，不做运行时动态启停。

- 详细实现计划：[`./modules/hooks.md`](./modules/hooks.md)

### 5.7 Agents 配置

MVP 阶段只保留单一 `AgentConfig`；多 profile / binding / fallback 设计后置到 Phase 2。

- 详细实现计划：[`./modules/agent-config.md`](./modules/agent-config.md)

---

## 6. 三类 Loop 摘要

### 6.1 Reviewer Loop

自动发现并认领待 review PR，调用 Agent 进行审查，并将结果同步回 GitHub。

- 详细实现计划：[`./modules/reviewer-loop.md`](./modules/reviewer-loop.md)

### 6.2 Worker Loop

围绕 `Spec + Checklist` 持续推进任务实现，直至创建 open PR。

- 详细实现计划：[`./modules/worker-loop.md`](./modules/worker-loop.md)

### 6.3 Fixer Loop

自动修复 PR 中的评论、CI、冲突等阻塞项，使 PR 回到可 merge 状态。

- 详细实现计划：[`./modules/fixer-loop.md`](./modules/fixer-loop.md)

---

## 7. API 与调度摘要

### 7.1 HTTP API

提供面向 CLI / UI 的统一接口，负责项目、任务、loop、run、PR 动作等操作。

- 详细实现计划：[`./modules/api.md`](./modules/api.md)

补充：需要提供统一的 `/api/v1/status` 聚合接口，供 `looper status` 使用。

### 7.2 Scheduler / Queue

将“发现工作”和“执行工作”解耦，通过队列、锁、重试、限流驱动三类 loop。

补充：scheduler 只负责“何时执行什么”，真正的 step 生命周期、恢复与取消统一交给 `LoopRunner`。

- 详细实现计划：[`./modules/scheduler.md`](./modules/scheduler.md)

---

## 8. 持久化摘要

MVP 使用 **SQLite**，并明确采用：

- `bun:sqlite`
- 原生 SQL
- 薄 repository
- SQL migration 文件
- **不引入 ORM**

- 详细设计：[`./storage.md`](./storage.md)
- Migration 设计：[`./modules/sqlite-migrations.md`](./modules/sqlite-migrations.md)

补充：npm 安装/升级后的 first-run boot flow 中，`looperd` 必须自动检查并执行 migrations，再进入正常服务阶段。

---

## 9. Git / Worktree / 通知 / 安全摘要

### 9.1 Git / Worktree

一个任务或 PR 对应一个 worktree，分支与任务绑定，禁止直接修改受保护分支。

- 详细实现计划：[`./modules/integrations.md`](./modules/integrations.md)

### 9.2 通知

第一期至少支持应用内通知与 macOS `osascript` 系统通知，并对关键自动化动作保留审计记录。

- 详细实现计划：[`./modules/integrations.md`](./modules/integrations.md)

### 9.3 安全

自动 approve、自动 merge、自动解决评论等高风险动作默认关闭，仅通过显式配置启用。

- 相关实现约束：[`./modules/integrations.md`](./modules/integrations.md)、[`./modules/looperd.md`](./modules/looperd.md)

---

## 10. MVP 范围

### 第一阶段（必须有）

- `looperd` HTTP 服务
- 单仓库配置
- Reviewer Loop 基础版
- Worker Loop 基础版
- Fixer Loop 第一阶段基础版
- SQLite 持久化
- 本地 worktree 管理
- 单一 Agent 适配
- CLI 基础能力

### 第二阶段

- Web UI
- CODEOWNERS reviewer 自动分配
- 更丰富的通知渠道（如飞书）
- 多 Agent 适配
- Fixer Loop 完整版（自动完成通知、更多修复策略、稳定重试与观察闭环）

### 第三阶段

- Webhook 驱动替代纯轮询
- 多仓库 / 多项目
- 更强的审计、权限和审批流
- 自动 merge / auto approve 策略化配置

---

## 11. 当前最重要的几个实现决策

1. **MVP 先单仓库**；多仓放到后续阶段，避免调度复杂度过早上升。
2. **默认全自动执行**；但任务阻塞、连续失败、显式 pause 时允许人工介入。
3. **Review 通过以 GitHub Approve 为强信号**；reaction / comment 仅作弱信号。
4. **Checklist 以存储实体为主**；后续再考虑与 markdown 双向同步。
5. **GitHub 数据源优先用 gh cli**；后续如稳定性或性能不足再切官方 API。
6. **looperd 先面向本机运行**；macOS 上推荐由 launchd 托管。
7. **三类 loop 共用同一执行底座**；避免 Reviewer / Worker / Fixer 各自复制状态机、重试与恢复逻辑。
8. **Agent 执行必须进程可控、可观测、可取消**；不接受“只拿 stdout/stderr 最终结果”的黑盒模型。
9. **MVP 采用 `1 Task : 1 PR : 1 Worker`**；先不支持多 task 汇入同一个 PR。
10. **用户主入口是 task / PR，不是 loop**；常规 task 流程中 reviewer / fixer 应由系统自动串联。
11. **Worker 主要负责 PR 创建前的推进**；PR 创建后的 review / fix 问题优先由 reviewer / fixer 接管。

---

## 12. 详细文档索引

- 主存储方案：[`./storage.md`](./storage.md)
- SQLite migrations：[`./modules/sqlite-migrations.md`](./modules/sqlite-migrations.md)
- looperd：[`./modules/looperd.md`](./modules/looperd.md)
- CLI / UI：[`./modules/clients.md`](./modules/clients.md)
- looperd 配置：[`./modules/config.md`](./modules/config.md)
- Hooks / Extension Bus：[`./modules/hooks.md`](./modules/hooks.md)
- 外部集成：[`./modules/integrations.md`](./modules/integrations.md)
- Agents 配置：[`./modules/agent-config.md`](./modules/agent-config.md)
- 领域模型：[`./modules/domain.md`](./modules/domain.md)
- Reviewer Loop：[`./modules/reviewer-loop.md`](./modules/reviewer-loop.md)
- Worker Loop：[`./modules/worker-loop.md`](./modules/worker-loop.md)
- Fixer Loop：[`./modules/fixer-loop.md`](./modules/fixer-loop.md)
- HTTP API：[`./modules/api.md`](./modules/api.md)
- Scheduler / Queue：[`./modules/scheduler.md`](./modules/scheduler.md)
- PR 自动发现机制：[`./pr-discovery.md`](./pr-discovery.md)

---

## 13. 建议的下一步

1. 先把 `packages/core` 的领域模型定下来
2. 初始化 `apps/looperd` 骨架与 SQLite store 接口
3. 先打通 Reviewer Loop 第一条链路
4. 再补 Worker，最后补 Fixer
