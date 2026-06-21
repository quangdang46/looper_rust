# 运行中任务速览（`looper ps`）方案

## 1. 背景

当前已经有：

- `looper loop list`
- `looper run list`
- `looper task status/show`
- `looper pr list/status`

但缺少一个“只看现在谁正在跑”的快速入口。

用户想回答的问题通常不是：

- 系统里一共有多少 loop / run

而是：

- 现在有没有 Worker 在干活
- 哪个 PR 正在被 Reviewer / Fixer 处理
- 当前卡在哪个 step
- 背后有没有真实 agent 进程在跑，PID 是多少，跑了多久

因此需要一个类似 `docker ps` 的命令，默认只展示**当前运行中的执行单元**。

---

## 2. 方案结论

新增一个顶级命令：

```bash
looper ps
```

它默认展示**当前处于 running 状态的 loop run**，并附带该 run 关联的活跃 agent execution 摘要。

设计原则：

1. **默认只看 active**，不把历史记录混进来
2. **展示单位以 loop run 为主**，而不是只看 agent process
3. **如果存在活跃 agent execution，则把 agent 信息并排展示**
4. **一屏可扫读**，优先解决“现在谁在跑”而不是“完整审计”
5. 详细历史继续留给 `run list` / `task show` / `pr status`

---

## 3. 为什么展示单位选 `run`，不是只看 agent execution

如果只列 `agent_executions`：

- 能看到 PID / vendor / heartbeat
- 但看不到完整业务语义
- 不能覆盖 `validate` / `push` / `resolve-comments` 这类程序化 step

而用户真正关心的是“哪个 Worker / Reviewer / Fixer 正在推进什么目标”。

所以 `looper ps` 应以 **active run** 为主表，再附加：

- loop 类型（worker / reviewer / fixer）
- target（task / PR）
- current step
- 当前 step 是否由 agent 驱动
- agent pid / vendor / heartbeat / startedAt

这样既保留“docker ps 式总览”，又不丢业务上下文。

---

## 4. CLI 设计

## 4.1 主命令

```bash
looper ps
```

默认输出建议：

| type | target | run | step | agent | pid | status | age |
| --- | --- | --- | --- | --- | --- | --- | --- |
| worker | task_12 | run_101 | implement | codex | 81234 | running | 12m |
| reviewer | repo#42 | run_102 | review | claude | 81288 | running | 4m |
| fixer | repo#42 | run_103 | validate | - | - | running | 1m |

字段说明：

- `type`: `worker` / `reviewer` / `fixer`
- `target`: task id 或 `repo#prNumber`
- `run`: run id
- `step`: 当前 step
- `agent`: 活跃 agent vendor；无则 `-`
- `pid`: 活跃 agent pid；无则 `-`
- `status`: run status，第一阶段只会主要看到 `running`
- `age`: 从 run.startedAt 到现在的时长

## 4.2 推荐 flags

第一阶段建议支持：

- `--json`：返回结构化数据
- `--type <worker|reviewer|fixer>`：按 loop type 过滤
- `--project <projectId>`：按项目过滤
- `--task <taskId>`：按 task 过滤
- `--pr <repo>#<number>`：按 PR 过滤
- `--watch`：每 2 秒刷新一次（可选，若首版想控 scope 可后置）
- `-a, --all`：展示最近一小段非活跃 run（第二阶段）

建议分阶段：

- **Phase 1**：`looper ps`、`--json`、`--type`、`--project`
- **Phase 2**：`--task`、`--pr`、`--watch`
- **Phase 3**：`--all`

## 4.3 命名选择

建议直接使用顶级命令 `looper ps`，而不是：

- `looper run active`
- `looper agent list-active`
- `looper status --running`

原因：

- `ps` 心智模型最短
- 与用户提出的 `docker ps` 诉求一致
- 适合作为“高频速览入口”

同时不替代已有命令：

- `looper ps` = 当前运行态速览
- `looper run list` = 历史 run 列表
- `looper task/pr status` = 单目标详情

---

## 5. 服务端接口设计

建议新增聚合接口：

```txt
GET /api/v1/runs/active
```

这里返回的是 **active run view**，本质上仍然是 `runs` 资源的聚合视图，而不是新增一个叫 `process` 的领域对象。

这样可以保持与现有模型一致：

- `RunRecord`
- `LoopRecord`
- `AgentExecutionRecord`

CLI 命令仍然叫：

```bash
looper ps
```

也就是：**CLI 用 `ps` 作为交互名称，API 仍用 `runs` 作为领域边界。**

返回形态建议：

```json
{
  "items": [
    {
      "runId": "run_101",
      "loopId": "loop_1",
      "projectId": "proj_1",
      "type": "worker",
      "status": "running",
      "currentStep": "implement",
      "startedAt": "2026-04-11T10:00:00.000Z",
      "target": {
        "type": "task",
        "taskId": "task_12",
        "label": "task_12"
      },
      "agent": {
        "active": true,
        "executionId": "agent_exec_1",
        "vendor": "codex",
        "pid": 81234,
        "startedAt": "2026-04-11T10:01:10.000Z",
        "lastHeartbeatAt": "2026-04-11T10:11:58.000Z",
        "heartbeatCount": 31,
        "status": "running"
      }
    }
  ]
}
```

查询参数建议：

- `active=true`（默认）
- `type=worker|reviewer|fixer`
- `projectId=...`
- `taskId=...`
- `repo=...&prNumber=...`

不建议第一阶段直接只返回原始 `/api/v1/runs` 记录，因为：

1. `runs` 目前只返回原始 run record
2. CLI 还需要 join loop / target / agent execution 才能形成可读视图
3. 让 CLI 拼装会导致逻辑分散到多个客户端

因此更合适的做法是由 looperd 提供一个**已聚合好的 active run view**，并把它挂在 `runs` 资源下。

---

## 6. 数据拼装逻辑

服务端聚合时，建议基于以下事实：

- `runs` 是当前 loop 执行态主记录
- `loops` 持有 loop type 和 target 信息
- `tasks` / `pull_requests` 提供 target 语义
- `agent_executions.listActive()` 能提供当前活跃 agent 进程

建议同时为 `runs` store 增加：

- `listByStatus(status: string)`

避免第一阶段通过 `runs.list()` 全量扫描历史记录后再过滤 `running`。

建议聚合算法：

1. 取 `runs.listByStatus("running")` 的记录
2. 按 `run.loopId` 关联 `loop`
3. 根据 `loop.targetType` 生成 `target.label`
   - task: 优先展示 task title，其次再回退 task id
   - PR: `<repo>#<prNumber>`
4. 取 `agent_executions.listActive()`，按 `runId` 建索引
5. 为每个 active run 附加最多一个“主活跃 agent execution”

补充约束：

- 响应中的 `type` 来自关联的 `loop.type`，不是 `run` 自身字段
- `age` 不应由服务端返回预格式化字符串，而应由 CLI 基于 `startedAt` 计算相对时长

关于“一条 run 是否会对应多个活跃 agent execution”：

- 第一阶段按系统现状应视为**不应发生**
- 若实际出现多个活跃 execution，接口应：
  - 仍返回该 run 一条记录
  - 选择最新 `startedAt` 的 execution 作为主展示对象
  - 同时返回 `agent.activeCount`

关于 `agentExecution.runId` 为空的情况：

- `AgentExecutionRecord.runId` 当前是 nullable
- 如果某个 active execution 还没有可靠绑定到 `runId`，则本接口 join 时应直接跳过
- 不应因为这类瞬时中间态导致 `/api/v1/runs/active` 失败

这样可以容忍异常态，又不让 CLI 表格爆炸。

---

## 7. 输出排序与可读性

`looper ps` 默认排序建议：

1. 有活跃 agent execution 的 run 排前面
2. 同组内按 `run.startedAt` 升序（跑最久的在前）

原因：

- 先让用户看到真正占着 agent 进程的任务
- 再让用户优先关注长时间运行项

当没有任何运行中任务时，输出：

```txt
No running loops.
```

不要输出空表。

---

## 8. 与现有命令的职责边界

## 8.1 不替代 `run list`

`run list` 保留“run 审计列表”角色；它可以继续偏原始。

`ps` 是高频操作视图，强调：

- active only
- 更强语义化 target
- 附加 agent 状态

## 8.2 不替代 `status`

`looper status` 仍然负责系统级健康、调度、总体统计。

`looper run list` 仍然负责 run 历史审计。

`looper ps` 负责回答：

- “现在谁在跑？”

补充：如果用户还想看“还有哪些工作在排队”，第一阶段仍优先查看 `looper status`；后续可给 `ps` 增加 queued footer 或 `--queued`。

## 8.3 为未来 Web UI 复用

`/api/v1/runs/active` 后续可直接供 Web UI 的“Running Now”面板使用。

因为 `apps/web` 目前还是占位实现，所以第一阶段先做 API + CLI 即可。

---

## 9. 实现范围建议

## Phase 1（推荐先做）

目标：先把“当前运行中任务一眼看清”做出来。

包含：

- 新增 `GET /api/v1/runs/active`
- 新增 `looper ps`
- 支持 `--json`
- 支持 `--type`、`--project`
- 表格输出 + 空态输出
- 基础测试

不包含：

- TUI / curses 风格刷新
- 复杂 watch 交互
- 历史 exited/failed 列表
- 杀进程 / 取消任务动作

## Phase 2

- `--task`
- `--pr`
- `--watch`
- 表格中增加 heartbeat lag / branch / repo 等可选列

## Phase 3

- `looper ps -a`
- 支持最近完成 / 失败项
- 支持 “stuck” 判断（如 heartbeat 超时高亮）

---

## 10. 测试建议

至少覆盖：

1. 没有 active run 时返回空列表
2. 有 active worker/reviewer/fixer 时能正确展示 type + target + step
3. 有活跃 agent execution 时能正确 join vendor/pid/heartbeat
4. run 在 running，但当前 step 无 agent 时，agent 列显示 `-`
5. `--type` / `projectId` 过滤正确
6. 多个 active agent execution 异常并存时，仍能稳定返回单条 run 记录
7. active agent execution 的 `runId` 为空时被安全跳过
8. task target 优先展示 title，缺失时回退 id

---

## 11. 后续可扩展点

如果后面要继续向 `docker ps` 靠拢，可以逐步增加：

- `COMMAND`：显示 agent command 摘要
- `NAMES`：显示更友好的 loop label
- `UPTIME`：区分 run uptime 与 agent uptime
- `STATE`：`running / waiting-agent / validating / pushing / stuck`
- queued footer / `--queued`
- `looper logs --run <id>` 快速跳日志
- `looper cancel <run-id>` 或 `looper kill <agent-exec-id>`
- `looper ps --stuck`
- `--watch` 后续可考虑 SSE，而不是只做轮询

但这些都不应阻塞第一阶段落地。

---

## 12. 最终建议

建议按以下最小闭环推进：

1. looperd 新增 `/api/v1/runs/active` 聚合视图
2. CLI 新增 `looper ps`
3. 默认只展示 running runs
4. 每行同时展示 loop 语义 + agent 摘要

这样能用最小改动解决当前痛点，并为后续 Web “Running Now”面板复用同一数据模型。
