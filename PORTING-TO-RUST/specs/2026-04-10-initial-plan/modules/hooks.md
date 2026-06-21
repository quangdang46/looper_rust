# Hooks / Extension Bus（目标架构）

## 1. 目标

把通知、审计、metrics、trace、调试输出等横切能力从 loop 核心逻辑里剥离，避免后续 `LoopRunner` 和 step handler 被日志与 side effect 污染。

注意：本文件描述的是**目标架构**。MVP 阶段可以不实现 HookBus，直接在主流程里调用 event log / notification。

---

## 2. 核心抽象（后续收敛时）

```ts
interface HookBus {
  emit<TEvent extends HookEvent>(event: TEvent): Promise<void>
  register<TEvent extends HookEvent>(hook: HookHandler<TEvent>): Dispose
}

interface HookHandler<TEvent extends HookEvent> {
  name: string
  handle(event: TEvent, ctx: HookContext): Promise<void>
}
```

约束：

- hook 默认不能阻塞主执行路径
- hook 执行失败必须被隔离并写审计日志
- 如果在 MVP 就实现，也只支持启动时静态注册，不做运行时动态启停

---

## 3. 生命周期事件

建议第一批内建事件：

- `loop.started`
- `loop.completed`
- `loop.failed`
- `loop.step.started`
- `loop.step.completed`
- `loop.step.failed`
- `run.heartbeat`
- `agent.started`
- `agent.heartbeat`
- `agent.completed`
- `agent.timeout`
- `notification.requested`
- `notification.sent`

---

## 4. 第一批内建 Hook（后续可选）

### 4.1 AuditHook

- 所有生命周期事件追加到 `event_logs`
- 写失败不影响主流程，但必须落 warning

### 4.2 NotificationHook

- 监听关键状态跃迁
- 把内部事件映射到 app / osascript 通知
- 内建 dedupe / throttle

### 4.3 MetricsHook（后置）

- 等 Web UI / 持续观测需求明确后再实现

### 4.4 DebugStreamHook（后置）

- 等需要可订阅事件流时再实现

---

## 5. 与 LoopRunner 的边界

- 如果实现 HookBus，则 `LoopRunner` 只负责 `emit(event)`
- 事件消费、聚合、通知、trace 由 HookBus 负责
- step handler 不直接耦合通知网关，必要时仅发领域事件

---

## 6. 安全与健壮性

- 一旦实现 HookBus，就必须使用 safe wrapper 执行每个 hook
- 单个 hook 超时或异常不能拖垮 loop 主流程
- 高风险 hook 默认不进入 MVP
