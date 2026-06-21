# Agents 配置（Phase 2 预研）

## 1. 目标

本文件降级为 **Phase 2 预研文档**。

MVP 阶段不实现 profile / binding / fallback 链，只需要在主配置里提供一个简单 agent 配置对象。

```ts
type AgentConfig = {
  vendor: 'claude-code' | 'codex' | 'opencode' | 'cursor-cli'
  model?: string
  params?: Record<string, unknown>
  env?: Record<string, string>
}
```

真正的 MVP 真相源应放在 `config.md`，而不是单独的持久化模型里。

未来如进入多 agent 阶段，这层配置才解决以下问题：

1. 不同项目可以选不同 coding agent
2. 同一 coding agent 可以定义多个 profile
3. Reviewer / Worker / Fixer 可以分别绑定不同 profile

---

## 2. 非 MVP 的候选数据模型

### 2.1 AgentProfile

```ts
type AgentVendor = 'claude-code' | 'codex' | 'opencode' | 'cursor-cli'

type AgentProfile = {
  id: string
  name: string
  vendor: AgentVendor
  description?: string
  model?: string
  enabled: boolean
  scope: 'global' | 'project'
  projectId?: string
  mode?: 'interactive' | 'headless'
  params: Record<string, unknown>
  env: Record<string, string>
  createdAt: string
  updatedAt: string
}
```

### 2.2 AgentBinding

```ts
type AgentBinding = {
  id: string
  projectId: string
  targetType: 'reviewer' | 'worker' | 'fixer'
  profileId: string
  fallbackProfileIds?: string[]
  createdAt: string
  updatedAt: string
}
```

---

## 3. 非 MVP 的配置优先级

建议优先级从高到低：

1. Run 级临时覆盖
2. Task / Loop 级指定 profile
3. Project 级 binding
4. Global 默认 profile
5. Agent 自身默认值

---

## 4. 非 MVP 的统一配置字段

```ts
type UnifiedAgentParams = {
  model?: string
  workingDirectory?: string
  resume?: 'none' | 'latest' | 'session-id'
  sessionId?: string
  outputFormat?: 'text' | 'json' | 'stream-json'
  approvalMode?: string
  sandboxMode?: string
  permissionMode?: string
  allowedTools?: string[]
  disallowedTools?: string[]
  extraWritableDirs?: string[]
  subagent?: string
  subagents?: unknown
  configProfile?: string
  timeoutMs?: number
  canWrite?: boolean
  canCommit?: boolean
  canPush?: boolean
}
```

vendor-specific 参数放在 `params` 中。

---

## 5. 各 Agent 可用参数调研摘要

### 5.1 Claude Code

- 关键参数：`--model`、`--permission-mode`、`--allowedTools`、`--disallowedTools`、`--tools`、`-p/--print`、`--output-format`、`--json-schema`、`--continue`、`--resume`、`--fork-session`、`--session-id`、`--name`、`--add-dir`、`--worktree`、`--agent`、`--agents`、`--bare`
- 关键环境变量：`ANTHROPIC_API_KEY`、`ANTHROPIC_MODEL`、`CLAUDE_CODE_SUBAGENT_MODEL`、`CLAUDE_CODE_EFFORT_LEVEL`、`ANTHROPIC_BASE_URL`
- 支持 subagent，且子代理可配置 `model / tools / disallowedTools / permissionMode / maxTurns / skills / background`
- 注意：subagent 不能再 spawn subagent

### 5.2 Codex CLI

- 关键参数：`--model/-m`、`--ask-for-approval/-a`、`--sandbox/-s`、`--yolo`、`--full-auto`、`codex exec --json`、`--output-last-message`、`--output-schema`、`--cd/-C`、`--add-dir`、`--profile/-p`、`codex resume`、`codex exec resume --last --all`、`--ephemeral`
- 关键配置/环境变量：`CODEX_HOME`、`CODEX_SQLITE_HOME`、`config.toml`、`.codex/config.toml`
- 支持 subagents，重点配置包括 `agents.max_threads`、`agents.max_depth`、`agents.job_max_runtime_seconds`
- 内置 agent：`default`、`worker`、`explorer`

### 5.3 OpenCode

- 关键参数：`--model/-m`、`permission`、`opencode run --format json`、`--continue/-c`、`--session/-s`、`--fork`、`--prompt`、`--file/-f`、`opencode serve`、`opencode run --attach`、`opencode acp --cwd`
- 关键配置/环境变量：`~/.config/opencode/opencode.json`、`opencode.json`、`OPENCODE_CONFIG`、`OPENCODE_CONFIG_DIR`、`OPENCODE_CONFIG_CONTENT`、`OPENCODE_PERMISSION`、`OPENCODE_ENABLE_EXA`
- 支持 agent / subagent：primary `build`、`plan`；subagent `general`、`explore`
- 还可配置 `default_agent`、`mode`、`permission.task`

### 5.4 Cursor CLI

- 关键参数：`--model`、`-p/--print`、`--output-format`、`--stream-partial-output`、`--resume <session-id>`、`--force`、`--yolo`
- 关键配置/环境变量：`CURSOR_API_KEY`、`~/.cursor/cli-config.json`、`<project>/.cursor/cli.json`、`CURSOR_CONFIG_DIR`
- 支持 subagents / background agents：`is_background: true`、`readonly: true`、`model: inherit | fast | <model>`
- 内置 subagents：`explore`、`bash`、`browser`

---

## 6. 非 MVP 的持久化结构候选

### 6.1 ClaudeCodeParams

```ts
type ClaudeCodeParams = {
  model?: string
  permissionMode?: 'default' | 'acceptEdits' | 'plan' | 'auto' | 'dontAsk' | 'bypassPermissions'
  outputFormat?: 'text' | 'json' | 'stream-json'
  allowedTools?: string[]
  disallowedTools?: string[]
  tools?: string[]
  addDirs?: string[]
  useWorktree?: boolean
  sessionMode?: 'new' | 'continue' | 'resume'
  sessionId?: string
  agent?: string
  agentsJson?: unknown
  bare?: boolean
}
```

### 6.2 CodexParams

```ts
type CodexParams = {
  model?: string
  approvalPolicy?: string
  sandboxMode?: string
  fullAuto?: boolean
  yolo?: boolean
  jsonOutput?: boolean
  outputSchemaPath?: string
  cd?: string
  addDirs?: string[]
  profile?: string
  resume?: 'none' | 'last' | 'session'
  sessionId?: string
  ephemeral?: boolean
}
```

### 6.3 OpenCodeParams

```ts
type OpenCodeParams = {
  model?: string
  permission?: 'allow' | 'ask' | 'deny'
  outputFormat?: 'text' | 'json'
  continue?: boolean
  sessionId?: string
  fork?: boolean
  prompt?: string
  files?: string[]
  attach?: boolean
  cwd?: string
  defaultAgent?: string
}
```

### 6.4 CursorCliParams

```ts
type CursorCliParams = {
  model?: string
  print?: boolean
  outputFormat?: 'text' | 'json' | 'stream-json'
  streamPartialOutput?: boolean
  resumeSessionId?: string
  force?: boolean
  yolo?: boolean
  workspace?: string
  subagent?: string
}
```

---

## 7. MVP 结论

第一阶段只做：

- `config.md` 中的单一 `AgentConfig`
- loop 执行时直接读取当前项目配置
- 不做 profile CRUD
- 不做 binding
- 不做 resolver
- 不做 fallback profile

当第二个 agent 适配真实接入并出现切换需求时，再回到本文件升级设计。

---

## 12. 主要参考来源

- Claude Code: https://docs.anthropic.com/en/docs/claude-code/cli-reference , https://docs.anthropic.com/en/docs/claude-code/headless , https://docs.anthropic.com/en/docs/claude-code/settings , https://code.claude.com/docs/en/env-vars , https://docs.anthropic.com/en/docs/claude-code/sub-agents , https://docs.anthropic.com/en/docs/claude-code/model-config
- Codex CLI: https://developers.openai.com/codex/cli/reference , https://developers.openai.com/codex/config-basic , https://developers.openai.com/codex/config-reference , https://developers.openai.com/codex/config-advanced , https://developers.openai.com/codex/cli/features , https://developers.openai.com/codex/subagents
- OpenCode: https://opencode.ai/docs/cli/ , https://opencode.ai/docs/config/ , https://opencode.ai/docs/models/ , https://opencode.ai/docs/agents/ , https://opencode.ai/docs/permissions/ , https://opencode.ai/docs/providers/
- Cursor CLI: https://cursor.com/en-US/blog/cli , https://cursor.com/docs/cli/reference/parameters , https://cursor.com/docs/cli/headless.md , https://cursor.com/docs/cli/reference/configuration , https://cursor.com/docs/subagents.md
