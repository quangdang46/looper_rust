# Running Task PS Implementation Checklist

## Phase 1: API Contract & Data Access

- [x] 新增 active runs 聚合接口约定
  - [x] 路由定为 `GET /api/v1/runs/active`
  - [x] 明确它是 `runs` 资源下的聚合视图，不引入新的 `process` 领域对象
  - [x] 明确 query 参数：`type`、`projectId`、`taskId`、`repo`、`prNumber`
  - [x] 明确返回字段：`runId`、`loopId`、`projectId`、`type`、`status`、`currentStep`、`startedAt`、`target`、`agent`

- [x] 扩展 `runs` store 查询能力
  - [x] 增加 `listByStatus(status: string)`
  - [x] SQLite store 增加对应查询实现
  - [x] 保持返回顺序稳定，便于上层排序/测试

- [x] 明确 active agent join 规则
  - [x] 使用 `agentExecutions.listActive()` 作为数据源
  - [x] `runId` 为空的 active execution 在 join 时直接跳过
  - [x] 多个 active execution 绑定同一 run 时，选择最新 `startedAt` 作为主展示对象
  - [x] 同时返回 `agent.activeCount` 以暴露异常并存情况

## Phase 2: Server Aggregation

- [x] 在 `apps/looperd/src/server/index.ts` 增加 `/api/v1/runs/active` 路由
  - [x] 仅支持 `GET`
  - [x] 读取并校验 query 参数
  - [x] 调用聚合 builder 返回 `items`

- [x] 实现 active run view 聚合逻辑
  - [x] 从 `runs.listByStatus("running")` 读取 active runs
  - [x] 按 `run.loopId` join `loop`
  - [x] 从 `loop.type` 派生响应里的 `type`
  - [x] 按 target 类型构造 `target` 结构
  - [x] task target 优先展示 task title，缺失时回退 task id
  - [x] PR target 展示为 `<repo>#<prNumber>`
  - [x] 挂载 active agent 摘要（vendor / pid / startedAt / lastHeartbeatAt / heartbeatCount / activeCount）

- [x] 实现过滤逻辑
  - [x] `type`
  - [x] `projectId`
  - [x] `taskId`
  - [x] `repo + prNumber`

- [x] 实现排序逻辑
  - [x] 有活跃 agent execution 的 run 排前面
  - [x] 同组内按 `run.startedAt` 升序

## Phase 3: CLI Command

- [x] 在 `apps/cli/src/index.ts` 增加顶级命令 `looper ps`
  - [x] 命令接入现有 CLI router
  - [x] 增加 help / example 文案
  - [x] 保持与现有 `run list` / `loop list` 风格一致

- [x] 支持 Phase 1 flags
  - [x] `--json`
  - [x] `--type <worker|reviewer|fixer>`
  - [x] `--project <projectId>`

- [x] 实现 CLI 输出
  - [x] 调用 `GET /api/v1/runs/active`
  - [x] 表格列输出：`type` / `target` / `run` / `step` / `agent` / `pid` / `status` / `age`
  - [x] `age` 由 CLI 基于 `startedAt` 计算相对时长
  - [x] 无活跃任务时输出 `No running loops.`
  - [x] 无 active agent 时 `agent` / `pid` 列显示 `-`

## Phase 4: Tests

- [x] Store 测试
  - [x] `runs.listByStatus("running")` 只返回目标状态 run
  - [x] 返回顺序稳定

- [x] API 测试
  - [x] 无 active run 时返回空列表
  - [x] 有 active worker / reviewer / fixer 时正确返回 type + target + currentStep
  - [x] active agent execution 正确 join 到 run
  - [x] 当前 step 无 agent 时返回空 agent 展示对象或 `null`
  - [x] active execution `runId` 为空时被安全跳过
  - [x] 多个 active execution 绑定同一 run 时只返回一条 run 记录，并带 `activeCount`
  - [x] `type` / `projectId` / `taskId` / `repo+prNumber` 过滤正确
  - [x] task target 缺 title 时正确回退到 id

- [x] CLI 测试
  - [x] `looper ps --json` 输出结构化结果
  - [x] 默认表格输出列顺序正确
  - [x] 空态输出为 `No running loops.`
  - [x] `--type` / `--project` 正确拼 query 参数

## MVP Cut Line

MVP 必须完成：

- [x] `GET /api/v1/runs/active`
- [x] `runs.listByStatus(status)`
- [x] active run + active agent 聚合视图
- [x] `looper ps`
- [x] `--json`
- [x] `--type` / `--project`
- [x] 空态输出
- [x] 基础 API / CLI 测试

后续再做：

- [ ] `--task`
- [ ] `--pr`
- [ ] `--watch`
- [ ] `looper ps -a`
- [ ] queued footer / `--queued`
- [ ] `looper ps --stuck`
- [ ] SSE-based watch
