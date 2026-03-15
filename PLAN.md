# PLAN.md — grove

> Rust CLI orchestrator for beads-driven multi-session Claude agent workflows.
> Each bead task = one Claude session. Context exhaustion triggers checkpoint + new session.
> Cross-session memory via cass + cm. Exit detection + circuit breaker patterns from ralph.

---

## 0. Research Findings

### 0.1 ralph-claude-code — Core loop logic, exit gate, circuit breaker

ralph (7.7k stars) is the most battle-tested autonomous Claude loop available. It solves the exact problem grove targets: keeping Claude running without human intervention. Grove adopts ralph's solutions directly.

**Key mechanisms grove adopts from ralph:**

**Dual-condition exit gate** — ralph's most important insight. Claude saying "done" is not enough to stop a loop. Exit requires BOTH:
1. `completion_indicators >= 2` (heuristic from natural language patterns)
2. Explicit `EXIT_SIGNAL: true` from Claude

This prevents premature exits during productive sessions. Grove adopts this as `GROVE_EXIT: true/false` in node output.

```
| completion_indicators | GROVE_EXIT | Result   |
|-----------------------|------------|----------|
| >= 2                  | true       | EXIT     |
| >= 2                  | false      | CONTINUE |
| >= 2                  | missing    | CONTINUE |
| < 2                   | true       | CONTINUE |
```

**Circuit breaker pattern** — ralph detects stuck loops and opens the circuit:
- No file changes for N loops → OPEN
- Same error repeated N times → OPEN  
- Output volume declining 70%+ → OPEN
- Auto-recovery: OPEN → cooldown → HALF_OPEN → CLOSED

Grove applies this per-node, not per-global-loop.

**Session continuity with `--resume`** — ralph uses `claude --resume <session_id>` to preserve context across loop iterations within the same session. Grove uses this for the within-session loop, then spawns a fresh session when context actually exhausts.

**Rate limit handling** — ralph has three-layer API limit detection (timeout guard → structured JSON `rate_limit_event` → filtered text fallback) and auto-waits in unattended mode. Grove adopts the same detection stack.

**`.ralphrc` / `grove.toml` config pattern** — ralph's `.ralphrc` approach: project-specific config file with sensible defaults. Grove uses `grove.toml` with the same philosophy.

**What ralph does NOT solve that grove adds:**
- ralph loops one session at a time, no DAG
- ralph has no cross-session memory (cass/cm)
- ralph cannot spawn a fresh session when context exhausts — it continues in the same session
- ralph has no parallel execution
- ralph has no task graph (beads)

### 0.2 beads_rust (`br`) — Stable JSON API, frozen format

Jeff created beads_rust specifically to freeze the "classic beads" architecture. The Go version (GasTown) is evolving; beads_rust is the stable interface.

From beads_rust AGENTS.md:
```
CRITICAL: Always use --json or --robot flags for programmatic parsing.
CORRECT: br ready --json
WRONG:   br list | head -1   ← TTY-dependent, format varies
```

**br commands grove uses:**
```bash
br ready --json                        # nodes with no blockers
br show <id> --json                    # full task details
br list --json                         # all tasks
br update <id> --status in_progress   # mark running
br close <id> --reason "<summary>"    # mark done
br dep add <child-id> <parent-id>     # add dependency
```

**BrIssue JSON schema:**
```json
{
  "id": "bd-abc123",
  "title": "Implement auth middleware",
  "description": "...",
  "priority": 1,
  "type": "task",
  "status": "open",
  "assignee": null,
  "labels": [],
  "blocked_by": [],
  "blocks": ["bd-def456"]
}
```

### 0.3 beads_viewer (`bv`) — DAG analytics

`bv` provides graph intelligence that `br` doesn't:
```bash
bv --robot-triage     # PageRank, critical path, parallel tracks
bv --robot-plan       # topological sort + execution order
bv --robot-graph      # export graph as JSON (used by grove web UI)
```

Grove uses `bv --robot-plan` to detect which nodes can run concurrently.

### 0.4 cass — Required, installed by grove

```bash
cass health                                              # exit 0=ready
cass search "<query>" --robot --limit 5 --mode hybrid   # search sessions
cass index                                               # incremental index
```

**Robot output:**
```json
{
  "results": [
    { "session_path": "...", "score": 0.95, "snippet": "...", "agent": "claude" }
  ],
  "total": 5
}
```

### 0.5 cm — Required, installed by grove

```bash
cm onboard status        # health check
cm recall "<context>"    # query rules/lessons → JSON
cm store "<lesson>"      # store after node done
cm serve --port 8765     # HTTP MCP mode (Phase 3, faster than CLI)
```

### 0.6 ntm — Context rotation approach

ntm estimates token usage via character count heuristic (~4 chars/token) since Claude Code doesn't expose token count directly. Grove uses the same heuristic. ntm's 3-tier escalation: warn (80%) → compact → rotate. Grove maps this to: warn → checkpoint → new session.

### 0.7 ccswarm — Workspace structure, PTY

ccswarm uses `crates/<name>/` workspace structure and `portable-pty` for subprocess spawning. Grove adopts the workspace structure, starts with `tokio::process::Command`, upgrades to PTY if `claude -p` requires TTY.

---

## 1. Architecture

```
grove/
├── Cargo.toml
├── grove.toml
├── install.sh
├── PLAN.md
├── README.md
└── crates/
    ├── grove-core/               # types, state machine, config
    │   └── src/
    │       ├── lib.rs
    │       ├── node.rs           # NodeId, NodeState, HandoffData, CheckpointData
    │       ├── dag.rs            # DagView — in-memory graph from br + bv
    │       └── config.rs         # GroveConfig from grove.toml
    │
    ├── grove-beads/              # br + bv integration
    │   └── src/
    │       ├── lib.rs
    │       ├── br_client.rs      # br CLI wrapper with JSON parsing
    │       ├── bv_client.rs      # bv --robot-* wrapper
    │       └── schema.rs         # BrIssue, BvPlan deserialization types
    │
    ├── grove-session/            # Claude session lifecycle
    │   └── src/
    │       ├── lib.rs
    │       ├── spawn.rs          # tokio::process spawn claude -p
    │       ├── monitor.rs        # context threshold heuristic (from ntm)
    │       ├── parser.rs         # parse GROVE_* markers from stdout
    │       ├── exit_gate.rs      # dual-condition exit (from ralph)
    │       └── circuit_breaker.rs # stuck loop detection (from ralph)
    │
    ├── grove-memory/             # cass + cm + handoff
    │   └── src/
    │       ├── lib.rs
    │       ├── cass_client.rs    # cass search --robot
    │       ├── cm_client.rs      # cm recall / cm store
    │       ├── handoff_store.rs  # atomic read/write handoff JSON
    │       └── context_builder.rs # assemble full prompt for each node
    │
    ├── grove-lock/               # parallel file safety
    │   └── src/
    │       ├── lib.rs
    │       ├── lock.rs           # fs2 advisory lock
    │       └── registry.rs       # node → lock tracking
    │
    ├── grove-orchestrator/       # main event loop
    │   └── src/
    │       ├── lib.rs
    │       ├── orchestrator.rs   # poll beads → spawn nodes → handle events
    │       ├── scheduler.rs      # dependency check, parallel track analysis
    │       ├── runner.rs         # tokio Semaphore task pool
    │       ├── checkpoint.rs     # checkpoint/resume session loop
    │       └── events.rs         # NodeEvent bus, events.jsonl audit log
    │
    ├── grove-web/                # embedded web UI (Phase 3)
    │   └── src/
    │       ├── lib.rs
    │       ├── server.rs         # axum HTTP server
    │       ├── api.rs            # REST + SSE endpoints
    │       └── assets/           # rust-embed: index.html + D3.js
    │
    └── grove-cli/                # binary entrypoint
        └── src/
            ├── main.rs
            └── commands/
                ├── run.rs        # grove run [--web]
                ├── status.rs     # grove status
                ├── tui.rs        # grove tui (ratatui)
                ├── log.rs        # grove log <node-id>
                ├── retry.rs      # grove retry <node-id>
                ├── tree.rs       # grove tree
                └── web.rs        # grove web [--port N]
```

---

## 2. Core Types (grove-core)

```rust
// node.rs

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeState {
    Pending,
    Ready,
    Running {
        session_id: String,
        started_at: DateTime<Utc>,
        attempt: u32,
    },
    Checkpointed {
        checkpoint: CheckpointData,
        attempt: u32,
    },
    Done {
        handoff: HandoffData,
        completed_at: DateTime<Utc>,
    },
    Failed {
        reason: String,
        attempts: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffData {
    pub node_id: NodeId,
    pub task_title: String,
    pub result_summary: String,
    pub artifacts: Vec<String>,
    pub git_commits: Vec<String>,
    pub key_decisions: Vec<String>,
    pub warnings: Vec<String>,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointData {
    pub node_id: NodeId,
    pub progress: String,
    pub next_step: String,
    pub context: serde_json::Value,
    pub attempt: u32,
    pub saved_at: DateTime<Utc>,
}

// dag.rs
pub struct DagView {
    pub nodes: HashMap<NodeId, BrIssue>,
    pub edges: Vec<(NodeId, NodeId)>,  // (parent, child)
}

impl DagView {
    pub fn all_parents_done(&self, node_id: &NodeId, done: &HashSet<NodeId>) -> bool {
        self.edges.iter()
            .filter(|(_, child)| child == node_id)
            .all(|(parent, _)| done.contains(parent))
    }
}
```

---

## 3. br + bv Integration (grove-beads)

```rust
// br_client.rs
pub struct BrClient {
    bin: PathBuf,
    project_dir: PathBuf,
}

impl BrClient {
    async fn run_json<T: DeserializeOwned>(&self, args: &[&str]) -> Result<T> {
        let out = Command::new(&self.bin)
            .args(args)
            .arg("--json")
            .current_dir(&self.project_dir)
            .output().await?;

        if !out.status.success() {
            return Err(anyhow!("br {} failed: {}", args[0],
                String::from_utf8_lossy(&out.stderr)));
        }
        Ok(serde_json::from_slice(&out.stdout)?)
    }

    pub async fn ready(&self) -> Result<Vec<BrIssue>>
    // br ready --json

    pub async fn list_all(&self) -> Result<Vec<BrIssue>>
    // br list --json

    pub async fn show(&self, id: &NodeId) -> Result<BrIssue>
    // br show <id> --json

    pub async fn mark_in_progress(&self, id: &NodeId) -> Result<()>
    // br update <id> --status in_progress

    pub async fn close(&self, id: &NodeId, summary: &str) -> Result<()>
    // br close <id> --reason "<summary>"
}

#[derive(Debug, Clone, Deserialize)]
pub struct BrIssue {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: u8,
    pub r#type: String,
    pub status: String,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub blocked_by: Vec<String>,
    pub blocks: Vec<String>,
}

// bv_client.rs
pub struct BvClient { bin: PathBuf, project_dir: PathBuf }

impl BvClient {
    pub async fn parallel_tracks(&self) -> Result<Vec<Vec<String>>>
    // bv --robot-plan --json → parse parallel execution tracks

    pub async fn export_graph(&self) -> Result<serde_json::Value>
    // bv --robot-graph --json → D3.js data for web UI
}
```

---

## 4. Session Management (grove-session)

### 4.1 Spawn

```rust
// spawn.rs
pub struct SessionConfig {
    pub node_id: NodeId,
    pub prompt: String,
    pub model: String,
    pub attempt: u32,
    pub session_id: Option<String>,   // for --resume (from ralph pattern)
}

pub async fn spawn_session(cfg: &SessionConfig, claude_bin: &Path) -> Result<ActiveSession> {
    let mut cmd = Command::new(claude_bin);
    cmd.args(["-p", &cfg.prompt, "--model", &cfg.model]);

    // Use --resume for same-session continuation (ralph pattern)
    // Start fresh when context exhausted (grove extension)
    if let Some(ref sid) = cfg.session_id {
        cmd.args(["--resume", sid]);
    }

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = cmd.spawn()?;

    Ok(ActiveSession { node_id: cfg.node_id.clone(), child, attempt: cfg.attempt })
}
```

### 4.2 Output Parser

```rust
// parser.rs
#[derive(Debug)]
pub enum SessionOutput {
    Line(String),
    Result {
        summary: String,
        artifacts: Vec<String>,
        lessons: Vec<String>,
        exit_signal: bool,       // GROVE_EXIT: true/false (from ralph EXIT_SIGNAL)
    },
    Checkpoint(CheckpointData),
    ExitSignal(bool),            // GROVE_EXIT: false = Claude says keep going
}

pub fn parse_line(line: &str, node_id: &NodeId) -> SessionOutput {
    if let Some(rest) = line.strip_prefix("GROVE_RESULT:") {
        // parse summary
    } else if let Some(rest) = line.strip_prefix("GROVE_ARTIFACTS:") {
        // parse comma-separated files
    } else if let Some(rest) = line.strip_prefix("GROVE_LESSONS:") {
        // parse lesson
    } else if let Some(rest) = line.strip_prefix("GROVE_EXIT:") {
        let exit = rest.trim().eq_ignore_ascii_case("true");
        return SessionOutput::ExitSignal(exit);
    } else if let Some(rest) = line.strip_prefix("GROVE_CHECKPOINT:") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(rest.trim()) {
            return SessionOutput::Checkpoint(CheckpointData {
                node_id: node_id.clone(),
                progress: v["progress"].as_str().unwrap_or("").to_string(),
                next_step: v["next_step"].as_str().unwrap_or("").to_string(),
                context: v["context"].clone(),
                attempt: 0,
                saved_at: Utc::now(),
            });
        }
    }
    SessionOutput::Line(line.to_string())
}
```

### 4.3 Dual-Condition Exit Gate (from ralph)

```rust
// exit_gate.rs
pub struct ExitGate {
    completion_indicators: u32,
    exit_signal: bool,
    threshold: u32,  // default 2 (from ralph)
}

impl ExitGate {
    pub fn update(&mut self, output: &SessionOutput) {
        match output {
            SessionOutput::ExitSignal(true) => self.exit_signal = true,
            SessionOutput::ExitSignal(false) => self.exit_signal = false,
            SessionOutput::Line(line) => {
                // Heuristic: natural language completion patterns (from ralph)
                let lower = line.to_lowercase();
                if lower.contains("all tasks complete")
                    || lower.contains("implementation complete")
                    || lower.contains("project ready")
                    || lower.contains("all done")
                {
                    self.completion_indicators += 1;
                }
            }
            _ => {}
        }
    }

    // Exit ONLY when BOTH conditions met
    pub fn should_exit(&self) -> bool {
        self.completion_indicators >= self.threshold && self.exit_signal
    }

    pub fn reset(&mut self) {
        self.completion_indicators = 0;
        self.exit_signal = false;
    }
}
```

### 4.4 Circuit Breaker (from ralph)

```rust
// circuit_breaker.rs
#[derive(Debug, Clone, PartialEq)]
pub enum CircuitState {
    Closed,     // normal operation
    Open,       // stuck detected, block further execution
    HalfOpen,   // testing recovery
}

pub struct CircuitBreaker {
    state: CircuitState,
    no_progress_count: u32,
    same_error_count: u32,
    last_error: Option<String>,
    last_file_count: usize,
    opened_at: Option<Instant>,

    // Thresholds (configurable via grove.toml, from ralph defaults)
    no_progress_threshold: u32,   // default 3
    same_error_threshold: u32,    // default 5
    cooldown: Duration,           // default 30min
}

impl CircuitBreaker {
    pub fn record_loop(&mut self, file_changes: usize, error: Option<&str>) {
        // No progress detection
        if file_changes == 0 {
            self.no_progress_count += 1;
        } else {
            self.no_progress_count = 0;
            self.last_file_count = file_changes;
        }

        // Same error detection (two-stage filter from ralph to avoid JSON false positives)
        if let Some(err) = error {
            if !self.is_json_field(err) {  // filter out JSON fields containing "error"
                if self.last_error.as_deref() == Some(err) {
                    self.same_error_count += 1;
                } else {
                    self.same_error_count = 1;
                    self.last_error = Some(err.to_string());
                }
            }
        }

        // Trip circuit
        if self.no_progress_count >= self.no_progress_threshold
            || self.same_error_count >= self.same_error_threshold
        {
            self.state = CircuitState::Open;
            self.opened_at = Some(Instant::now());
        }
    }

    pub fn is_blocked(&mut self) -> bool {
        match self.state {
            CircuitState::Closed => false,
            CircuitState::Open => {
                // Auto-recovery after cooldown
                if self.opened_at.map(|t| t.elapsed() >= self.cooldown).unwrap_or(false) {
                    self.state = CircuitState::HalfOpen;
                    self.reset_counts();
                    false  // allow one attempt
                } else {
                    true
                }
            }
            CircuitState::HalfOpen => false,
        }
    }

    pub fn on_success(&mut self) {
        self.state = CircuitState::Closed;
        self.reset_counts();
    }

    fn is_json_field(&self, err: &str) -> bool {
        // Avoid ralph's false positive: JSON {"error": "..."} triggering error detection
        err.trim_start().starts_with('"') || err.contains("\\\"error\\\"")
    }
}
```

### 4.5 Context Monitor (from ntm)

```rust
// monitor.rs
pub struct ContextMonitor {
    estimated_tokens: u32,
    threshold_pct: u8,
    model_limit: u32,
}

impl ContextMonitor {
    pub fn record(&mut self, chars: usize) {
        // ~4 chars per token (ntm heuristic)
        self.estimated_tokens += (chars / 4) as u32;
    }

    pub fn usage_pct(&self) -> u8 {
        ((self.estimated_tokens * 100) / self.model_limit).min(100) as u8
    }

    pub fn should_warn(&self) -> bool {
        self.usage_pct() >= self.threshold_pct
    }
}

pub fn model_limit(model: &str) -> u32 {
    200_000  // all current Claude models
}
```

---

## 5. Memory Integration (grove-memory)

### 5.1 cass Client

```rust
// cass_client.rs
pub struct CassClient { bin: PathBuf }

impl CassClient {
    pub async fn health(&self) -> bool {
        Command::new(&self.bin).arg("health")
            .status().await.map(|s| s.success()).unwrap_or(false)
    }

    pub async fn search(&self, query: &str, limit: u8) -> Result<Vec<CassResult>> {
        let out = Command::new(&self.bin)
            .args(["search", query, "--robot", "--mode", "hybrid",
                   "--limit", &limit.to_string(), "--fields", "minimal"])
            .output().await?;
        let r: CassResponse = serde_json::from_slice(&out.stdout)?;
        Ok(r.results)
    }

    pub async fn index_incremental(&self) -> Result<()> {
        // Call after each node done to keep cass fresh
        Command::new(&self.bin).arg("index").output().await?;
        Ok(())
    }

    pub async fn search_for_task(&self, issue: &BrIssue, limit: u8) -> Result<String> {
        let query = format!("{} {}", issue.title,
            issue.description.as_deref().unwrap_or("").chars().take(100).collect::<String>());
        let results = self.search(&query, limit).await?;
        if results.is_empty() { return Ok("(no relevant past sessions)".into()); }
        Ok(results.iter()
            .map(|r| format!("[{:.2}] {}", r.score, r.snippet))
            .collect::<Vec<_>>().join("\n\n"))
    }
}

#[derive(Deserialize)] pub struct CassResponse { pub results: Vec<CassResult> }
#[derive(Deserialize)] pub struct CassResult {
    pub score: f32,
    pub snippet: String,
    pub agent: String,
}
```

### 5.2 cm Client

```rust
// cm_client.rs
pub struct CmClient { bin: PathBuf }

impl CmClient {
    pub async fn health(&self) -> bool {
        Command::new(&self.bin).args(["onboard", "status"])
            .status().await.map(|s| s.success()).unwrap_or(false)
    }

    pub async fn recall(&self, ctx: &str) -> Result<String> {
        let out = Command::new(&self.bin).args(["recall", ctx]).output().await?;
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    pub async fn store(&self, lesson: &str) -> Result<()> {
        Command::new(&self.bin).args(["store", lesson]).output().await?;
        Ok(())
    }
}
```

### 5.3 Handoff Store

```rust
// handoff_store.rs
pub struct HandoffStore { dir: PathBuf }

impl HandoffStore {
    pub async fn write(&self, h: &HandoffData) -> Result<()> {
        let path = self.dir.join(format!("handoff_{}.json", h.node_id.0));
        let tmp = path.with_extension("json.tmp");
        tokio::fs::write(&tmp, serde_json::to_string_pretty(h)?).await?;
        tokio::fs::rename(tmp, path).await?;  // atomic
        Ok(())
    }

    pub async fn read(&self, id: &NodeId) -> Result<Option<HandoffData>> {
        let path = self.dir.join(format!("handoff_{}.json", id.0));
        if !path.exists() { return Ok(None); }
        Ok(Some(serde_json::from_slice(&tokio::fs::read(path).await?)?))
    }

    pub async fn read_parents(&self, ids: &[NodeId]) -> Result<Vec<HandoffData>> {
        let mut out = vec![];
        for id in ids {
            if let Some(h) = self.read(id).await? { out.push(h); }
        }
        Ok(out)
    }
}
```

### 5.4 Context Builder

```rust
// context_builder.rs
impl ContextBuilder {
    pub async fn build(&self, issue: &BrIssue, checkpoint: Option<&CheckpointData>) -> Result<String> {
        let parent_ids = issue.blocked_by.iter().map(|s| NodeId(s.clone())).collect::<Vec<_>>();
        let handoffs = self.handoff_store.read_parents(&parent_ids).await?;
        let cass_ctx = self.cass.search_for_task(issue, self.cfg.cass_search_limit).await
            .unwrap_or_else(|_| "(cass unavailable)".into());
        let cm_ctx = self.cm.recall(&issue.title).await
            .unwrap_or_else(|_| "(cm unavailable)".into());

        let resume = checkpoint.map(|cp| format!(
            "\n[RESUME FROM CHECKPOINT]\nProgress: {}\nNext step: {}\n",
            cp.progress, cp.next_step
        )).unwrap_or_default();

        Ok(format!(r#"[GROVE NODE]
ID: {id}
Task: {title}
Priority: P{priority}
Parents done: {parents}
{resume}
[PARENT OUTPUTS]
{handoffs}

[RELEVANT PAST SESSIONS]
{cass}

[AGENT MEMORY]
{cm}

[TASK]
{desc}

[GROVE PROTOCOL]
On completion output ALL of:
  GROVE_RESULT: <one-line summary>
  GROVE_ARTIFACTS: <comma-separated files, or "none">
  GROVE_LESSONS: <one lesson, or "none">
  GROVE_EXIT: true

While still working, output periodically:
  GROVE_EXIT: false

If context is filling up, checkpoint before it's too late:
  GROVE_CHECKPOINT: {{"progress": "...", "next_step": "...", "context": {{}}}}
"#,
            id=issue.id, title=issue.title, priority=issue.priority,
            parents=parent_ids.iter().map(|p| p.0.as_str()).collect::<Vec<_>>().join(", "),
            resume=resume,
            handoffs=format_handoffs(&handoffs),
            cass=cass_ctx, cm=cm_ctx,
            desc=issue.description.as_deref().unwrap_or("(no description)"),
        ))
    }
}
```

---

## 6. Orchestrator (grove-orchestrator)

### 6.1 Main Loop

```rust
// orchestrator.rs
impl Orchestrator {
    pub async fn run(&self) -> Result<()> {
        self.check_deps().await?;

        loop {
            let ready = self.br.ready().await?;
            let states = self.states.read().await;
            let to_spawn: Vec<BrIssue> = ready.into_iter()
                .filter(|n| !matches!(states.get(&NodeId(n.id.clone())),
                    Some(NodeState::Running { .. })))
                .collect();
            drop(states);

            for issue in to_spawn {
                if self.semaphore.available_permits() > 0 {
                    self.spawn_node(issue).await?;
                }
            }

            // Exit when no open beads remain
            let all = self.br.list_all().await?;
            if all.iter().all(|n| n.status == "closed") { break; }

            tokio::time::sleep(Duration::from_secs(self.cfg.orchestrator.poll_interval_secs)).await;
        }
        Ok(())
    }
}
```

### 6.2 Node Execution Loop

This is where ralph's logic applies at the per-node level:

```rust
// checkpoint.rs
pub async fn run_node(issue: &BrIssue, ctx: &NodeContext) -> Result<HandoffData> {
    let mut attempt = 0u32;
    let mut checkpoint: Option<CheckpointData> = None;
    let mut circuit = CircuitBreaker::new(&ctx.cfg);

    loop {
        attempt += 1;
        if attempt > ctx.cfg.orchestrator.retry_max {
            return Err(anyhow!("Node {} exceeded max retries", issue.id));
        }

        if circuit.is_blocked() {
            tracing::warn!("Node {} circuit OPEN — waiting for cooldown", issue.id);
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }

        // Build prompt (with checkpoint if resuming after context exhaust)
        let prompt = ctx.ctx_builder.build(issue, checkpoint.as_ref()).await?;

        let mut session = spawn_session(&SessionConfig {
            node_id: NodeId(issue.id.clone()),
            prompt,
            model: ctx.cfg.session.default_model.clone(),
            attempt,
            session_id: None,  // fresh session (context exhausted from previous)
        }, &ctx.cfg.session.claude_bin).await?;

        let mut monitor = ContextMonitor::new(
            ctx.cfg.orchestrator.context_threshold,
            model_limit(&ctx.cfg.session.default_model),
        );
        let mut exit_gate = ExitGate::new(2);  // threshold=2 from ralph
        let mut result_parts: Option<ResultParts> = None;
        let mut file_changes = 0usize;

        // Read output line by line
        let outcome = loop {
            tokio::select! {
                line = session.next_line() => {
                    match line? {
                        None => {
                            let exit_code = session.wait().await?;

                            // Three-layer API limit detection (from ralph)
                            if session.stderr_contains("rate_limit_event")
                                || session.stderr_contains("5-hour")
                                || exit_code == 124  // timeout, not API limit
                            {
                                if exit_code == 124 {
                                    // timeout, not API limit — ralph v0.11.5 fix
                                    break Outcome::Timeout;
                                }
                                tracing::warn!("API rate limit hit. Waiting 60s...");
                                tokio::time::sleep(Duration::from_secs(60)).await;
                                break Outcome::RateLimit;
                            }

                            if let Some(parts) = result_parts {
                                break Outcome::Done(parts.into_handoff(issue));
                            }
                            break Outcome::Failed("Session ended without GROVE_RESULT".into());
                        }
                        Some(raw) => {
                            monitor.record(raw.len());
                            let output = parse_line(&raw, &NodeId(issue.id.clone()));
                            exit_gate.update(&output);

                            match &output {
                                SessionOutput::Result { summary, artifacts, lessons, exit_signal } => {
                                    result_parts = Some(ResultParts {
                                        summary: summary.clone(),
                                        artifacts: artifacts.clone(),
                                        lessons: lessons.clone(),
                                    });
                                    if *exit_signal {
                                        // Dual-condition check: indicators + EXIT_SIGNAL
                                        if exit_gate.should_exit() {
                                            break Outcome::Done(result_parts.take().unwrap().into_handoff(issue));
                                        }
                                    }
                                    // EXIT_SIGNAL: false → CONTINUE even if result written
                                }
                                SessionOutput::Checkpoint(cp) => {
                                    checkpoint = Some(cp.clone());
                                    session.kill().await?;
                                    break Outcome::Checkpoint;
                                }
                                SessionOutput::Line(line) => {
                                    // Count file changes for circuit breaker
                                    if line.contains("Wrote ") || line.contains("Created ") {
                                        file_changes += 1;
                                    }
                                    // Broadcast for grove log / web UI
                                    ctx.broadcast_log(&NodeId(issue.id.clone()), line.clone());

                                    if monitor.should_warn() {
                                        tracing::warn!(
                                            "Node {} at {}% context. Watching for GROVE_CHECKPOINT.",
                                            issue.id, monitor.usage_pct()
                                        );
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ = tokio::time::sleep(
                    Duration::from_secs(ctx.cfg.session.timeout_minutes * 60)
                ) => {
                    session.kill().await?;
                    break Outcome::Timeout;
                }
            }
        };

        // Update circuit breaker
        circuit.record_loop(file_changes, None);

        match outcome {
            Outcome::Done(handoff) => {
                circuit.on_success();
                return Ok(handoff);
            }
            Outcome::Checkpoint => {
                // Spawn new session with checkpoint injected
                tracing::info!("Node {} checkpointed. Spawning fresh session.", issue.id);
                continue;
            }
            Outcome::RateLimit => {
                // Wait already done above, retry
                continue;
            }
            Outcome::Timeout | Outcome::Failed(_) => {
                if attempt >= ctx.cfg.orchestrator.retry_max {
                    return Err(anyhow!("Node {} failed after {} attempts", issue.id, attempt));
                }
                tokio::time::sleep(Duration::from_secs(ctx.cfg.orchestrator.retry_backoff_secs)).await;
                continue;
            }
        }
    }
}
```

### 6.3 Post-Node Completion

```rust
async fn on_node_done(&self, handoff: HandoffData) {
    // 1. Atomic write handoff file
    self.memory.handoff_store.write(&handoff).await.ok();

    // 2. Store lessons in cm
    for lesson in &handoff.key_decisions {
        self.memory.cm.store(lesson).await.ok();
    }

    // 3. Incremental cass index (so child nodes can search this session)
    self.memory.cass.index_incremental().await.ok();

    // 4. Mark closed in beads
    self.br.close(&handoff.node_id, &handoff.result_summary).await.ok();

    // 5. Update local state
    self.states.write().await.insert(
        handoff.node_id.clone(),
        NodeState::Done { handoff: handoff.clone(), completed_at: Utc::now() }
    );

    // 6. Emit event (web UI SSE + TUI)
    let _ = self.event_tx.send(NodeEvent::Done(handoff.node_id));
}
```

---

## 7. Lock Coordination (grove-lock)

From `claude_code_agent_farm` pattern:

```rust
// lock.rs
use fs2::FileExt;

pub struct FileLock { _file: std::fs::File, path: PathBuf }

impl FileLock {
    pub fn try_acquire(resource: &str, node_id: &NodeId, dir: &Path) -> Result<Option<Self>> {
        let key = format!("{:x}", sha2::Sha256::digest(resource.as_bytes()));
        let path = dir.join(format!("{}.lock", &key[..16]));
        let file = OpenOptions::new().create(true).write(true).open(&path)?;
        match file.try_lock_exclusive() {
            Ok(_) => {
                // Write metadata for debugging
                serde_json::to_writer(&file, &json!({
                    "node_id": node_id.0, "resource": resource,
                    "acquired_at": Utc::now().to_rfc3339()
                }))?;
                Ok(Some(FileLock { _file: file, path }))
            }
            Err(_) => Ok(None)
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.path); }
}
```

---

## 8. Web UI (grove-web) — Phase 3

### API

```
GET  /                → serve embedded index.html
GET  /api/nodes       → all nodes + states
GET  /api/nodes/:id   → node detail: task, handoff, config
GET  /api/dag         → bv --robot-graph JSON for D3.js
GET  /api/config      → grove.toml as JSON
PUT  /api/config      → update grove.toml fields
GET  /api/events      → SSE stream: NodeEvent (started, logline, done, failed)
POST /api/retry/:id   → retry failed node
```

### Frontend

Single `index.html` embedded via `rust-embed`. No build step required.
- D3.js force-directed DAG: gray=pending, blue=running, green=done, red=failed
- Click node → side panel: task description + live log tail + handoff JSON
- Config panel: grove.toml fields editable in-browser
- SSE for live updates — no polling

```rust
// server.rs
pub async fn start(port: u16, state: Arc<GroveState>) -> Result<()> {
    let app = Router::new()
        .route("/", get(serve_index))
        .route("/api/nodes", get(api_nodes))
        .route("/api/nodes/:id", get(api_node_detail))
        .route("/api/dag", get(api_dag))
        .route("/api/config", get(api_get_config).put(api_set_config))
        .route("/api/events", get(api_sse))
        .route("/api/retry/:id", post(api_retry))
        .with_state(state);

    println!("Grove web UI → http://127.0.0.1:{}", port);
    axum::Server::bind(&format!("127.0.0.1:{}", port).parse()?)
        .serve(app.into_make_service()).await?;
    Ok(())
}
```

---

## 9. Install Script

```bash
#!/usr/bin/env bash
set -euo pipefail

echo "=== grove installer ==="

# 1. grove binary
cargo install grove 2>/dev/null || \
  cargo install --git https://github.com/quangdang46/grove

# 2. bv (beads_viewer)
if ! command -v bv &>/dev/null; then
    echo "Installing bv..."
    cargo install --git https://github.com/Dicklesworthstone/beads_viewer
fi

# 3. cass
if ! command -v cass &>/dev/null; then
    echo "Installing cass..."
    curl -fsSL \
      "https://raw.githubusercontent.com/Dicklesworthstone/coding_agent_session_search/main/install.sh" \
      | bash
fi

cass health &>/dev/null || { echo "Running initial cass index..."; cass index || true; }

# 4. cm
if ! command -v cm &>/dev/null; then
    echo "Installing cm..."
    curl -fsSL \
      "https://raw.githubusercontent.com/Dicklesworthstone/cass_memory_system/main/install.sh" \
      | bash
fi

# 5. Check br (user must install manually — not on crates.io yet)
if ! command -v br &>/dev/null; then
    echo ""
    echo "ERROR: br (beads_rust) not found. Install it first:"
    echo "  cargo install --git https://github.com/Dicklesworthstone/beads_rust"
    exit 1
fi

echo ""
echo "=== grove ready ==="
echo "  cd <your-project>"
echo "  br init && grove run"
```

---

## 10. grove.toml

```toml
[orchestrator]
max_parallel = 5
context_threshold = 80
poll_interval_secs = 5
retry_max = 3
retry_backoff_secs = 30

# Circuit breaker (from ralph defaults)
cb_no_progress_threshold = 3
cb_same_error_threshold = 5
cb_cooldown_minutes = 30

[session]
claude_bin = "claude"
default_model = "sonnet"
timeout_minutes = 60

[memory]
handoff_dir = ".grove/handoffs"
lock_dir = ".grove/locks"
cass_search_limit = 5
cass_days = 30

[beads]
br_bin = "br"
bv_bin = "bv"

[web]
port = 3030
host = "127.0.0.1"
```

---

## 11. Dependencies

```toml
# grove-core
serde = { version = "1", features = ["derive"] }
serde_json = "1"
chrono = { version = "0.4", features = ["serde"] }
toml = "0.8"
anyhow = "1"
thiserror = "1"

# grove-session
tokio = { version = "1", features = ["full"] }
# portable-pty = "0.8"  ← upgrade from tokio::process if claude needs TTY

# grove-lock
fs2 = "0.4"
sha2 = "0.10"

# grove-orchestrator
tokio = { version = "1", features = ["full"] }
futures = "0.3"
tracing = "0.1"
tracing-subscriber = "0.3"

# grove-web (Phase 3)
axum = "0.7"
rust-embed = "8"
tower-http = { version = "0.5", features = ["cors"] }

# grove-cli
clap = { version = "4", features = ["derive"] }
ratatui = "0.28"
indicatif = "0.17"
```

---

## 12. Implementation Phases

### Phase 1 — Sequential MVP

- [ ] grove-core: all types, GroveConfig, DagView
- [ ] grove-beads: BrClient (all commands), BrIssue schema
- [ ] grove-session: spawn, parser (GROVE_*), ExitGate (dual-condition), CircuitBreaker, ContextMonitor
- [ ] grove-memory: CassClient, CmClient, HandoffStore, ContextBuilder
- [ ] grove-orchestrator: sequential loop, checkpoint/resume, rate limit handling
- [ ] grove-cli: `grove run`, `grove status`
- [ ] install.sh

**Milestone:** Sequential beads DAG end-to-end: spawn → exit gate → checkpoint/resume → memory → close.

### Phase 2 — Parallel

- [ ] grove-lock: FileLock, LockRegistry
- [ ] grove-beads: BvClient (parallel_tracks)
- [ ] grove-orchestrator: Semaphore pool, NodeEvent broadcast, parallel spawn
- [ ] grove-cli: `grove tui` (ratatui), `grove log`, `grove retry`, `grove tree`

**Milestone:** Parallel DAG, zero file conflicts, live TUI.

### Phase 3 — Web UI

- [ ] grove-web: axum, REST + SSE, embedded index.html, D3.js DAG
- [ ] grove-memory: cm HTTP mode (cm serve)
- [ ] grove-orchestrator: graceful shutdown (checkpoint all running), events.jsonl
- [ ] grove-cli: `grove web`, `grove run --web`

**Milestone:** Production-ready with web UI.

---

## 13. What Grove Takes from Each Repo

| Mechanism | Source | Grove Application |
|-----------|--------|------------------|
| Dual-condition exit gate | ralph | Per-node exit: GROVE_EXIT + completion_indicators |
| Circuit breaker | ralph | Per-node stuck detection + auto-recovery |
| Rate limit detection (3-layer) | ralph v0.11.5 | API limit handling in session runner |
| Session resume (`--resume`) | ralph | Within-session loop before context exhaust |
| `--json` stable API | beads_rust | All br calls use --json, format frozen |
| Parallel track detection | bv | Scheduler reads `bv --robot-plan` |
| DAG export for UI | bv | Web UI reads `bv --robot-graph` |
| Token heuristic (4 chars/token) | ntm | ContextMonitor estimation |
| 3-tier escalation (warn→compact→rotate) | ntm | warn (80%) → checkpoint → new session |
| PTY spawn approach | ccswarm | Phase 2 upgrade if needed |
| Workspace `crates/<name>/` | ccswarm | Grove workspace structure |
| File-based advisory lock | agent_farm | grove-lock for parallel safety |

---

## 14. Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| `claude -p` requires TTY (fails non-interactive) | Test first; fall back to `portable-pty` |
| Token heuristic off by large margin | Default 80% threshold (conservative); user can lower to 60% |
| cass index stale after node completes | Grove calls `cass index` after every node done |
| Exit gate triggers too early | `GROVE_EXIT: false` from Claude always wins; threshold=2 required |
| Circuit breaker false positive on JSON fields | Two-stage filter (ralph v0.11.5 fix) strips JSON context |
| Parallel nodes corrupt shared files | grove-lock advisory lock on every write |
| br close fails transiently | Retry 3× with 5s backoff; log failure; continue |
| GROVE_CHECKPOINT appears mid-sentence | Only parse lines starting exactly with `GROVE_CHECKPOINT:` |
| API 5-hour limit hit mid-run | Three-layer detection (ralph pattern); auto-wait 60min in unattended mode |

---

## 15. Success Metrics

- [ ] Phase 1: 10-node sequential DAG completes correctly, checkpoint/resume works across context exhaust
- [ ] Phase 2: 5 parallel nodes, zero file conflicts, TUI shows live state
- [ ] Phase 3: Web UI renders live DAG, streams node logs in real-time
- [ ] Memory: child node uses context from parent handoff + cass search
- [ ] Exit gate: no premature exits when GROVE_EXIT=false
- [ ] Circuit breaker: stuck nodes detected and retried without human intervention
- [ ] Orchestrator overhead: < 2s per node transition
