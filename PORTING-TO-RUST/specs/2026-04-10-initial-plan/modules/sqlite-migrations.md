# SQLite Migration 详细实现计划

## 1. 目标

为 Looper 提供轻量、可审计、可自动执行的 SQLite schema migration 机制。

约束：

- 不依赖 ORM migration
- 不依赖外部 migration 服务
- 由 `looperd` 启动时自动执行
- 必须兼容 npm 分发后的自动升级场景

---

## 2. 核心原则

1. **只做前向 migration**
2. **每个 migration 一个 SQL 文件**
3. **按序号严格执行**
4. **每次执行前先备份数据库**
5. **失败则阻止服务启动**

---

## 3. 元数据表

```sql
CREATE TABLE IF NOT EXISTS schema_migrations (
  id TEXT PRIMARY KEY,
  applied_at TEXT NOT NULL
);
```

---

## 4. 文件结构

```txt
apps/looperd/src/storage/sqlite/migrations/
  0001_init.sql
  0002_add_queue_items.sql
  0003_add_agent_profiles.sql
```

命名建议：

- `<4-digit>_<name>.sql`

---

## 5. 启动执行流程

1. 打开 SQLite 连接
2. 确保 `schema_migrations` 存在
3. 扫描 migrations 目录
4. 读取已执行 migration 集合
5. 按序执行未应用 migration
6. 每个 migration 成功后写入 `schema_migrations`
7. 全部成功后继续启动 looperd

补充要求：

- migration 检查必须发生在 HTTP server 与 scheduler 启动前
- 任何 pending migration 都应自动执行，而不是提示用户手工跑升级命令

---

## 6. 执行规则

- 每个 migration 必须放在事务中执行
- migration 文件应尽量幂等，但仍以 `schema_migrations` 为主判据
- 若任一 migration 失败：
  - 停止后续执行
  - 输出错误日志
  - 阻止服务进入 running

同时要求：

- 每个已发布 npm 版本都必须包含其所需的全部历史 migration 文件
- migration 文件一旦发布，不得被修改，只能新增更高版本文件

---

## 7. SQLite ALTER 限制处理

当需要复杂变更时，采用标准重建表策略：

1. 新建临时表
2. 拷贝数据
3. 删除旧表
4. 重命名新表

---

## 8. 示例

### 8.1 初始 migration

```sql
BEGIN;

CREATE TABLE projects (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  repo_path TEXT NOT NULL,
  base_branch TEXT,
  archived INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE loops (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  type TEXT NOT NULL,
  target_type TEXT NOT NULL,
  target_id TEXT,
  repo TEXT,
  pr_number INTEGER,
  status TEXT NOT NULL,
  last_run_at TEXT,
  next_run_at TEXT
);

CREATE TABLE runs (
  id TEXT PRIMARY KEY,
  loop_id TEXT NOT NULL,
  status TEXT NOT NULL,
  current_step TEXT,
  last_completed_step TEXT,
  checkpoint_json TEXT,
  last_heartbeat_at TEXT,
  started_at TEXT NOT NULL,
  ended_at TEXT
);

CREATE TABLE tasks (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  title TEXT NOT NULL,
  spec_path TEXT,
  pr_number INTEGER,
  status TEXT NOT NULL
);

CREATE TABLE task_items (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL,
  content TEXT NOT NULL,
  status TEXT NOT NULL,
  source TEXT NOT NULL
);

CREATE TABLE pull_request_snapshots (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  repo TEXT NOT NULL,
  pr_number INTEGER NOT NULL,
  head_sha TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  captured_at TEXT NOT NULL
);

CREATE TABLE locks (
  key TEXT PRIMARY KEY,
  owner TEXT NOT NULL,
  expires_at TEXT NOT NULL
);

CREATE TABLE event_logs (
  id TEXT PRIMARY KEY,
  event_type TEXT NOT NULL,
  entity_type TEXT,
  entity_id TEXT,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);

COMMIT;
```

### 8.2 增加 queue_items

```sql
BEGIN;

CREATE TABLE queue_items (
  id TEXT PRIMARY KEY,
  type TEXT NOT NULL,
  target_id TEXT NOT NULL,
  dedupe_key TEXT NOT NULL,
  scheduled_at TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_queue_items_scheduled_at ON queue_items (scheduled_at);
CREATE UNIQUE INDEX idx_queue_items_dedupe_key ON queue_items (dedupe_key);

COMMIT;
```

---

## 9. 接口建议

```ts
interface MigrationRunner {
  listPending(): Promise<string[]>
  runPending(): Promise<void>
}
```

---

## 10. 运维与排障

- `looper daemon status` 应显示当前 schema 版本
- 启动失败时应明确输出失败的 migration 文件名
- migration 前生成带时间戳的 db 备份

---

## 11. npm 分发下的升级语义

### 11.1 基本原则

- npm 包升级 = 代码升级
- 首次启动新版本 `looperd` = 自动执行 schema 升级
- 用户不需要单独运行 `db upgrade` 才能继续使用

### 11.2 兼容性要求

- 新版代码必须能识别旧版 schema
- 如存在 pending migrations，必须在启动阶段自动补齐
- 如 migration 失败，应保持旧 db 备份可回滚排查

### 11.3 推荐可观测性

建议提供：

- `looper db status`
- `looper doctor`
- 启动日志里输出：当前 schema 版本 / 目标版本 / pending migrations 数量

### 11.4 first-run boot flow 约束

在 npm 分发场景下，首次安装或首次升级启动时，migration runner 必须满足：

1. 自动执行，不依赖用户手工升级 db
2. 先迁移，再启动 HTTP server 与 scheduler
3. 首次安装时可从空数据库初始化
4. 升级启动时必须先备份旧 db
5. 失败时明确暴露失败 migration 文件名与备份路径

推荐把 first-run / upgrade-run 都视为同一个 boot pipeline，只在“是否已有旧 db”和“是否存在 pending migrations”上分支。

---

## 12. 非目标

当前阶段先不做：

- down migration
- 自动生成 migration
- 跨数据库兼容 migration
- ORM 驱动 migration
