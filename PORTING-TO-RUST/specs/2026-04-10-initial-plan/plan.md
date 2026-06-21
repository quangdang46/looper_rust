# Looper Implementation Plan

## 1. 仓库与应用骨架

- [x] 创建 `apps/looperd/src/` 目录骨架（`bootstrap/`、`server/`、`app/`、`domain/`、`infra/`、`storage/`、`runtime/`）
- [x] 创建 `apps/cli/` 基础骨架
- [x] 预留 `apps/web/` 目录但不实现 MVP UI
- [x] 配置 Bun + TypeScript 基础运行与构建脚本
- [x] 配置 monorepo 根级脚本（dev、typecheck、test、lint）

## 2. 配置与启动

- [x] 定义 `LooperConfig` 与 `AgentConfig` 的 TypeScript 类型
- [x] 实现配置加载顺序（CLI / env / `~/.looper/config.json`）
- [x] 实现配置校验
- [x] 实现工具路径探测（`bun`、`git`、`gh`、`osascript`）
- [x] 实现 `looperd` foreground 启动入口
- [x] 实现启动日志初始化

## 3. SQLite 与持久化基础

- [x] 接入 `bun:sqlite`
- [x] 创建 migration runner
- [x] 编写 `0001_init.sql`
- [x] 建立最小核心表：`loops`
- [x] 建立最小核心表：`runs`
- [x] 建立最小核心表：`locks`
- [x] 建立最小核心表：`event_logs`
- [x] 建立最小核心表：`pull_request_snapshots`
- [x] 建立最小核心表：`tasks`
- [x] 建立最小核心表：`task_items`
- [x] 为 `loops` 表加入 target 字段（`target_type` / `target_id` / `repo` / `pr_number`）
- [x] 实现数据库初始化与 WAL / foreign_keys / busy_timeout 设置
- [x] 实现数据库备份入口
- [x] 先用 `db.ts` + 具名函数落地最小读写能力
- [x] 为关键写操作提供事务封装

## 4. 核心领域与状态

- [x] 定义 `Project`、`Loop`、`Run`、`Task`、`TaskItem`、`PullRequestSnapshot`、`Lock` 类型
- [x] 定义 `LoopType`、`LoopStatus`、`RunStatus` 等值对象
- [x] 定义 `LoopTarget` 模型（`task` / `pull_request`）
- [x] 明确 `Loop` 与 `Run` 的 1:N 关系约束
- [x] 明确 Worker / Reviewer / Fixer 各自绑定的 target 类型
- [x] 明确 `loops` 表如何表达 task target 与 pull_request target
- [x] 明确同一 `project + type + target` 只能有一个 active loop 的约束
- [x] 明确 `running -> terminal` 只能发生一次的约束
- [x] 明确 MVP 下一个 task 最多关联一个 PR
- [x] 明确 MVP 下一个 PR 最多关联一个 task
- [x] 明确 Reviewer / Fixer 对同一 PR 保持一对一运行
- [x] 明确 PR 锁 key 规则（`pr:{repo}:{pr}`）
- [x] 明确 Task 锁 key 规则（`task:{taskId}`）
- [x] 实现审计事件追加模型

## 5. looperd 启动、恢复与运行时

- [x] 实现 `looperd` runtime 启动流程
- [x] 实现启动时自动 migration
- [x] 实现恢复流程：清理 orphan agent process
- [x] 实现恢复流程：清理过期锁
- [x] 实现恢复流程：标记中断 run
- [x] 实现恢复流程：校正 queue / loop 状态
- [x] 实现恢复事件写入
- [x] 实现进程停止流程
- [x] 实现 agent 子进程清理流程

## 6. HTTP API 基础

- [x] 搭建 Bun HTTP server
- [x] 实现统一 `/api/v1` 路由前缀
- [x] 实现统一 API response envelope
- [x] 实现 `GET /api/v1/healthz`
- [x] 实现 `GET /api/v1/status`
- [x] 实现 `GET /api/v1/config`
- [x] 实现 `GET /api/v1/events`
- [x] 实现 `GET /api/v1/events/:entityType/:entityId`
- [x] 实现 `GET /api/v1/pull-requests`
- [x] 实现 `GET /api/v1/pull-requests/:repo/:prNumber`
- [x] 实现 `GET /api/v1/pull-requests/:repo/:prNumber/status`
- [x] 实现基础错误码映射
- [x] 预留本地 token 鉴权但允许 MVP 先本机运行

## 7. CLI 基础

- [x] 实现 `looper status`
- [x] 实现 `looper config show`
- [x] 实现 `looper daemon status`
- [x] 实现 `looper daemon logs`
- [x] 实现 `looper loop list`
- [x] 实现 `looper loop start`
- [x] 实现 `looper loop pause`
- [x] 实现 `looper task create`
- [x] 实现 `looper task start`
- [x] 实现 `looper task pause`
- [x] 实现 `looper task status`
- [x] 实现 `looper task show`
- [x] 实现 `looper pr list`
- [x] 实现 `looper pr show`
- [x] 实现 `looper pr status`
- [x] 实现 `looper run list`

## 8. 外部集成基础

### 8.1 GitHub

- [x] 封装 `gh pr list`
- [x] 封装 `gh pr view`
- [x] 封装 `gh pr diff` 或等价 snapshot 获取方式
- [x] 封装 `gh pr review`
- [x] 封装 PR 评论 / 状态读取的最小能力

### 8.2 Git / Worktree

- [x] 封装 worktree 创建
- [x] 封装 worktree 查询与恢复
- [x] 封装分支创建与绑定
- [x] 封装工作区清理
- [x] 实现禁止直接修改受保护分支的保护逻辑

### 8.3 Agent

- [x] 实现单一 Agent 适配入口
- [x] 实现 agent 子进程启动
- [x] 实现 agent 超时控制
- [x] 实现 agent 取消与 kill
- [x] 实现 stdout / stderr 捕获
- [x] 实现 agent 结果解析
- [x] 实现 agent 执行审计落盘
- [x] 记录 agent pid 供恢复时清理

### 8.4 通知

- [x] 实现应用内通知落盘
- [x] 实现 macOS `osascript` 通知
- [x] 实现关键失败 / action required 场景通知

## 9. Scheduler / Queue 基础

- [x] 实现轮询式 scheduler
- [x] 实现 queue item 数据结构
- [x] 实现 enqueue
- [x] 实现 dequeue
- [x] 实现 scheduled item 查询
- [x] 实现 attempt 计数与指数退避
- [x] 实现 retryable / non-retryable / manual_intervention 分类
- [x] 实现业务锁获取与释放
- [x] 实现 reviewer 优先于 fixer 的 PR 抢占规则
- [x] 实现 pause / cancel 对调度层的影响

## 10. Reviewer Loop（先打通第一条链路）

- [x] 实现 reviewer discover
- [x] 实现 reviewer filter
- [x] 实现 reviewer claim
- [x] 实现 reviewer snapshot
- [x] 实现 reviewer review（调用 agent）
- [x] 实现 reviewer publish（回写 GitHub review）
- [x] 保证 publish 失败时可重试而不重复 review 同一 head sha
- [x] 为 reviewer loop 写入 run / event / snapshot / lock 状态
- [x] 实现 reviewer loop 的线性执行主流程
- [x] 实现 reviewer loop 的失败处理与重试
- [x] 实现 reviewer loop 的最小恢复语义（从最后成功 step 的下一步继续）

## 11. Worker Loop（MVP 基础版）

- [x] 实现 worker `prepare-task`
- [x] 实现 worker `prepare-worktree`
- [x] 实现 worker `plan-step`
- [x] 实现 worker `execute-step`
- [x] 实现 worker `validate-step`
- [x] 实现 worker `sync-checklist`
- [x] 实现 worker `open-pr`
- [x] 实现 `openPrStrategy`（`all_done` / `first_commit` / `manual`）
- [x] 实现 checklist slice -> run 的迭代模型
- [x] 实现 worker 的 worktree 生命周期管理
- [x] 实现 worker 的 PR 创建流程

## 12. Fixer Loop（第一阶段基础版）

- [x] 实现 fixer `discover-pr`
- [x] 实现 fixer `claim-pr`
- [x] 实现 fixer `collect-fixes`
- [x] 实现 fixer `repair`
- [x] 实现 fixer `validate`
- [x] 实现 fixer `push`
- [x] 实现 fixer `recheck`
- [x] 实现 FixItem 快照模型
- [x] 实现 fixer 与 reviewer 的 PR 互斥规则
- [x] 实现 fixer 基础失败恢复

## 13. 审计、日志与状态可见性

- [x] 为 loop 生命周期写审计事件
- [x] 为 run 生命周期写审计事件
- [x] 为 agent 调用写审计事件
- [x] 为 PR 回写动作写审计事件
- [x] 为通知发送写审计事件
- [x] 让 `/api/v1/status` 聚合 loop / run / scheduler / tools 状态
- [x] 让 CLI `looper status` 直接消费聚合状态接口

## 14. 安全与策略开关

- [x] 默认关闭 auto approve
- [x] 默认关闭 auto merge
- [x] 默认关闭高风险自动修复动作
- [x] 配置化控制 commit / push 权限
- [x] 配置化控制通知开关
- [x] 对受保护分支写操作加防护

## 15. 测试与验证

- [x] 为配置加载与校验写测试
- [x] 为 migration runner 写测试
- [x] 为 SQLite 关键读写写测试
- [x] 为锁语义写测试
- [x] 为 scheduler 重试与退避写测试
- [x] 为 reviewer loop 第一条链路写集成测试
- [x] 为 worker 基础链路写集成测试
- [x] 为 fixer 基础链路写集成测试
- [x] 为恢复流程写测试
- [x] 为 agent timeout / kill 写测试

## 16. MVP 验收 checklist

- [x] `looperd` 能本地启动并通过 `/healthz`
- [x] `looperd` 首次启动会自动执行 migrations
- [x] CLI 能查看系统状态与配置
- [x] reviewer loop 能发现 PR、调用 agent、成功回写 review
- [x] worker loop 能基于 checklist 推进任务并创建 PR
- [x] fixer loop 能处理基础修复场景并回推结果
- [x] 所有核心状态都能落 SQLite
- [x] 关键副作用都有审计记录
- [x] 中断后可恢复到一致状态
- [x] 受保护分支不会被直接修改
