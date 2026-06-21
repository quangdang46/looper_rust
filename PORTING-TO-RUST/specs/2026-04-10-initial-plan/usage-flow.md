# Looper 用户使用流程

本文档不是实现设计，而是站在用户视角说明：Looper 该怎么被使用、日常怎么运转、用户在什么节点介入。

---

## 1. Looper 对用户来说是什么

Looper 是一个本地常驻服务。

用户不直接操作 GitHub、Git、Agent CLI 的底层细节，而是通过：

- CLI
- Web UI（后续）

把下面三类工作交给后台持续执行：

1. **Reviewer**：自动 review PR
2. **Worker**：围绕 task 持续推进实现
3. **Fixer**：自动修复 PR 上的阻塞项

---

## 2. 整体使用心智模型

用户主要接触三个对象：

- **Task**：要完成什么
- **PR**：代码最终在哪个 pull request 上协作
- **Loop**：Looper 在后台持续跑的自动化流程

推荐理解方式：

- `Task` 是工作单元
- `PR` 是协作与交付单元
- `Loop` 是后台执行单元

其中：

- 一个 **Worker loop** 通常围绕一个 task 运转
- 一个 **Reviewer loop** 围绕一个 PR 运转
- 一个 **Fixer loop** 围绕一个 PR 运转
- MVP 阶段采用 **1 Task : 1 PR : 1 Worker**

---

## 3. 第一次使用流程

### 3.1 安装与准备

用户完成：

1. 安装 Looper
2. 本机准备好：
   - `git`
   - `gh`
   - 一个支持的 agent CLI
3. 准备 Looper 配置文件

### 3.2 启动服务

用户执行：

```bash
looper status
```

预期行为：

1. 如果 `looperd` 未启动，Looper 启动或提示启动
2. 自动检查本地配置
3. 自动检查 SQLite
4. 自动执行 migrations
5. 返回系统状态

### 3.3 用户确认系统可用

用户应该能看到：

- 服务是否正在运行
- 当前配置是否有效
- agent / git / gh 是否可用
- 数据库是否正常
- 当前有哪些 loop / run

---

## 4. 日常使用流程总览

日常主要有两种入口：

### 4.1 从 PR 出发

适合：

- 让系统自动 review 一个 PR
- 让系统持续修复一个已有 PR

典型流程：

1. 用户创建或选择一个 PR
2. Looper 为该 PR 启动 reviewer loop
3. 如果 PR 出现评论、失败 CI、冲突等问题
4. Looper 为该 PR 启动或唤醒 fixer loop

### 4.2 从 Task 出发

适合：

- 从需求 / spec / todo 出发推进开发

典型流程：

1. 用户创建 task
2. task 下有一组 checklist（领域层叫 task items）
3. Looper 为该 task 启动 worker loop
4. worker 持续推进实现
5. worker 在合适时机创建 PR
6. reviewer / fixer 围绕这个 PR 自动串联

---

## 5. 用户旅程一：创建 Task 并交给 Worker 推进

这是 Looper 最核心的开发流程。

### 5.1 创建 task

用户执行：

```bash
looper task create
```

用户提供：

- task 标题
- 可选 spec 路径
- 初始 checklist / task items

结果：

- 系统创建一个 task
- task 进入可执行状态

### 5.2 启动 task

用户执行：

```bash
looper task start <task-id>
```

结果：

- 系统创建一个 worker loop
- loop target 绑定到该 task
- scheduler 开始调度 worker run
- 当 worker 创建 PR 后，系统自动创建或唤起 reviewer loop
- 当 PR 出现阻塞项时，系统自动创建或唤起 fixer loop

### 5.3 Worker 在后台做什么

一次典型 worker run 会做：

1. 读取 task 和当前未完成 task items
2. 准备 worktree
3. 选择本轮要推进的 1~2 个 task items
4. 调用 agent 实现
5. 本地验证
6. 更新 task items 状态
7. 如有需要，创建或更新 PR

### 5.4 用户何时介入

用户通常在这些时候介入：

- task 需要补充/修改 task items
- worker 连续失败
- agent 做偏了，需要人工纠偏
- 用户希望暂停某个 task

用户可以做的动作：

- `looper task pause <task-id>`
- `looper task status <task-id>`
- 更新 task / task items
- 重新启动 task
- 手工修改代码后让 Looper 继续

---

## 6. 用户旅程二：让 Reviewer 自动审查 PR

### 6.1 创建或选择 PR

用户已经有一个 PR，或者 worker 已自动创建 PR。

### 6.2 启动 reviewer loop

用户执行：

```bash
looper loop start --type reviewer --pr <repo>#<number>
```

说明：这主要用于“已有 PR 托管”场景。对于正常 task 驱动开发，reviewer loop 应由 worker 创建 PR 后自动串联。

### 6.3 Reviewer 在后台做什么

一次典型 reviewer run 会做：

1. 发现待处理 PR
2. 过滤不该 review 的 PR
3. 获取 PR 锁
4. 生成 PR snapshot
5. 调用 agent review
6. 把 review 结果发布回 GitHub

### 6.4 用户看到什么

用户会看到：

- PR 上的 review comment / approve / request changes
- Looper 中的 reviewer run 历史
- 失败时的原因与重试状态

### 6.5 关键约束

- 同一个 PR 同时只有一个 active reviewer loop
- publish 失败时可以重试
- 不应该因为 publish 失败而重复 review 同一 head sha

---

## 7. 用户旅程三：让 Fixer 自动修 PR

### 7.1 何时启动 fixer

当 PR 出现这些情况时：

- review comments 需要处理
- CI 失败
- merge conflict
- 其他使 PR 不能 merge 的阻塞项

### 7.2 启动 fixer loop

用户执行：

```bash
looper loop start --type fixer --pr <repo>#<number>
```

说明：这主要用于“已有 PR 托管”场景。对于正常 task 驱动开发，fixer loop 应在 PR 出现阻塞项后自动串联。

### 7.3 Fixer 在后台做什么

一次典型 fixer run 会做：

1. 发现 PR 当前阻塞项
2. 获取 PR 锁
3. 归一化成 FixItem 列表
4. 调用 agent 修复
5. 本地验证
6. push 修复结果
7. recheck PR 健康状态

### 7.4 与 Reviewer / Worker 的关系

- fixer 和 reviewer 都是围绕 PR 的
- 同一个 PR 上它们需要遵守锁规则，避免互相打架
- Worker 主要负责 PR 创建前的推进与本地验证
- PR 创建后，围绕该 PR 的 review / fix 问题优先由 reviewer / fixer 接管

---

## 8. 用户如何查看系统状态

### 8.1 看整体状态

用户执行：

```bash
looper status
```

期望看到：

- 服务状态
- 配置状态
- 工具可用性
- 当前活跃 loop
- 最近 runs
- 失败/阻塞摘要

### 8.2 看具体 loop / run

用户执行：

```bash
looper pr list
looper pr show <repo>#<number>
looper pr status <repo>#<number>
looper loop list
looper run list
looper task status <task-id>
```

用户可以理解：

- 哪个 task / PR 正在被处理
- 当前处于哪个阶段
- 最近一次失败在哪里
- 是否正在重试

补充：对于大多数用户，`pr` 维度通常比 `loop` 维度更贴近日常关心对象。

### 8.3 看配置

用户执行：

```bash
looper config show
```

用于确认：

- 当前 agent 配置
- 当前项目配置
- 工具路径
- 默认策略开关

---

## 9. 用户如何介入与纠偏

Looper 不是全黑盒自动机，用户应该能随时接管。

### 9.1 暂停

当用户觉得当前 loop 需要停止时：

```bash
looper loop pause <loop-id>
```

适用场景：

- agent 方向跑偏
- PR 需要人工整理
- task 目标变更

### 9.2 修改任务定义

用户可以修改：

- task 描述
- task items
- PR 关联关系

然后再让 loop 继续。

### 9.3 手工修改代码后继续

Looper 需要支持这种真实场景：

1. worker / fixer 跑到一半
2. 用户自己手工改一部分
3. 再让 Looper 继续后续工作

### 9.4 失败后恢复

用户不需要理解底层恢复机制，但应感知到：

- Looper 会尽量从中断点继续
- 不会无脑重复副作用
- 实在无法安全恢复时，会进入需要人工介入的状态

---

## 10. 推荐的日常使用模式

### 模式 A：Task 驱动开发

适合大多数日常需求。

1. 创建 task
2. 启动 task
3. worker 创建/更新 PR
4. reviewer 自动审查
5. fixer 在需要时自动修复
6. 用户只在关键节点介入

### 模式 B：已有 PR 托管

适合团队已有开发流程，只想把 review/fix 自动化。

1. 用户已有 PR
2. 启动 reviewer
3. 启动 fixer
4. 持续修到 PR 可 merge

## 11. 用户预期中的系统边界

用户不应该预期 Looper：

- 自动替代所有产品决策
- 自动合并高风险改动
- 在受保护分支上直接写代码
- 无限制地并发修改同一个 PR

用户应该预期 Looper：

- 能持续推进重复性开发流程
- 能围绕 task 和 PR 保持状态一致
- 能在失败后留下清晰状态与审计记录
- 能允许人工随时接管

---

## 12. 一句话总结推荐流程

推荐的整体使用方式是：

**用户创建 task → worker 持续推进并汇入一个 PR → reviewer 审查这个 PR → fixer 修复这个 PR → 用户只在需要决策和纠偏时介入。**
