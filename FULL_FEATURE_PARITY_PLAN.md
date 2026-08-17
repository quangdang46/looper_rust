# Looper Rust — Full Feature Parity Plan

> **Objective:** Bring `looper_rust` to full feature parity with the Go original (`nexu-io/looper`), then exceed it.
> **Generated:** 2026-08-17 | **Baseline:** Go `nexu-io/looper` @ HEAD, Rust `quangdang46/looper_rust` @ b420ba9

---

## Table of Contents

1. [Current State Summary](#1-current-state-summary)
2. [Phase 0 — Quick Wins & Documentation](#2-phase-0--quick-wins--documentation)
3. [Phase 1 — Takeover Feature (P0)](#3-phase-1--takeover-feature-p0)
4. [Phase 2 — Web Dashboard (P0)](#4-phase-2--web-dashboard-p0)
5. [Phase 3 — Forgejo/Gitea Provider (P1)](#5-phase-3--forgejogitae-provider-p1)
6. [Phase 4 — HITL Workflows (P1)](#6-phase-4--hitl-workflows-p1)
7. [Phase 5 — Advanced PR Management (P1)](#7-phase-5--advanced-pr-management-p1)
8. [Phase 6 — Agent Skills & CLI Polish (P2)](#8-phase-6--agent-skills--cli-polish-p2)
9. [Phase 7 — CI/CD & Testing Hardening (P2)](#9-phase-7--cicd--testing-hardening-p2)
10. [Phase 8 — Extras Beyond Go](#10-phase-8--extras-beyond-go)
11. [Dependency Map](#11-dependency-map)
12. [Effort Estimates](#12-effort-estimates)

---

## 1. Current State Summary

### What Rust already has (and does better)

| Area | Status | Notes |
|------|--------|-------|
| 5 runner roles (Coordinator, Planner, Reviewer, Worker, Fixer) | ✅ Full | Step pipelines, dispatch, merge-watch |
| 6 agent vendors (Claude, OpenAI, Gemini, Grok, DeepSeek, Custom) | ✅ Full | Native resume, two-tier timeout, SIGTERM→SIGKILL |
| SQLite storage (13 repos, 5 migrations) | ✅ Full | Better abstraction than Go |
| REST API + SSE (30+ endpoints) | ✅ Full | Axum-based |
| Webhook forwarding | ✅ Full | Worker pool, routing, tunnel |
| Config (3-layer merge, TOML/YAML/JSON) | ✅ Full | Better validation than Go |
| Notification system | ✅ Full | 5 channels vs Go's 1 |
| Disclosure stamps | ✅ Full | Configurable |
| Bootstrap wizard | ✅ Full | 14-step |
| Auto-upgrade | ✅ Full | GitHub releases |
| Network mode (LooperNet) | ✅ Full | Coordinator, node join |
| Dispatch access control | ✅ Full | Human-gated vs autonomous |
| Docker/Nix tool sandbox | ✅ Config only | Config exists, runtime impl needed |
| E2E test suite | ✅ Partial | Fake binaries, fewer scenarios |

### What Rust is missing (Go has, Rust doesn't)

| # | Feature | Priority | Est. Effort |
|---|---------|----------|-------------|
| 1 | Takeover feature (CLI + daemon + single-PR focus) | **P0** | 2-3 weeks |
| 2 | Web Dashboard (React+Vite SPA) | **P0** | 3-4 weeks |
| 3 | Forgejo/Gitea provider | **P1** | 2-3 weeks |
| 4 | HITL (Human-in-the-Loop) workflows | **P1** | 1-2 weeks |
| 5 | Auto-merge strategy selection (Squash/Merge/Rebase) | **P1** | 3-5 days |
| 6 | Reviewer: change-request dismissal | **P1** | 2-3 days |
| 7 | PR template + Review checklist | **P2** | 1 day |
| 8 | ADR documents (16 ADRs) | **P2** | 2-3 days |
| 9 | Skills (AI agent skill files) | **P2** | 2-3 days |
| 10 | Changelogs | **P2** | 1 day |
| 11 | Sandbox E2E CI workflow | **P2** | 3-5 days |
| 12 | `run-stats` / `logs-follow` CLI commands | **P2** | 2-3 days |
| 13 | Plane provider integration | **P2** | 1 week |

### Hidden stubs that need real implementation

| Stub | What it should do |
|------|-------------------|
| `takeover` | Single-PR takeover mode — focus daemon on one PR |
| `run-stats` | Aggregate run statistics (duration, success rate, tokens) |
| `logs-follow` | Real-time log streaming for a run |
| `netadmin` | Network admin operations |
| `labels` | Manage looper labels across repos |
| `prompt` | View/edit agent prompts |
| `feedback` | Submit feedback on agent output |
| `webhook` | Webhook management (status, cleanup, rotate, orphans, delete) |

---

## 2. Phase 0 — Quick Wins & Documentation

**Duration:** 2-3 days
**Goal:** Repo hygiene, documentation parity, zero-risk improvements

### 2.1 GitHub Repo Hygiene

| Task | Details | Files |
|------|---------|-------|
| PR template | Port from Go `.github/pull_request_template.md` | `.github/pull_request_template.md` |
| Code review checklist | Port from Go `.github/code-review-checklist.md` | `.github/code-review-checklist.md` |
| Issue templates | Bug report + feature request templates | `.github/ISSUE_TEMPLATE/` |

### 2.2 Documentation

| Task | Details | Files |
|------|---------|-------|
| ADR-0001: Coordinator is stateless | Port all 16 ADRs | `docs/adr/0001-*.md` through `docs/adr/0016-*.md` |
| Changelog | Create `CHANGELOG.md` + `docs/changelogs/` | Track from first release |
| Installation docs | Port `docs/installation.md` | `docs/installation.md` |
| Users guide | Port `docs/users-guide.md` | `docs/users-guide.md` |
| Loopernet deployment | Port `docs/loopernet-deployment.md` | `docs/loopernet-deployment.md` |

### 2.3 Agent Skills

| Task | Details | Files |
|------|---------|-------|
| Looper skill | Port `skills/looper/SKILL.md` + references | `skills/looper/SKILL.md`, `skills/looper/references/*.md` |
| PR takeover skill | Port `skills/pr-takeover/SKILL.md` | `skills/pr-takeover/SKILL.md` |
| Skill check script | Port `skills/looper/scripts/check.sh` | `skills/looper/scripts/check.sh` |

### 2.4 Code Conventions

| Task | Details |
|------|---------|
| Pre-commit hook | Port `.githooks/pre-commit` for `cargo fmt` + `cargo clippy` |
| Verify script | Create `scripts/verify.sh` (fmt + clippy + test + build) |

---

## 3. Phase 1 — Takeover Feature (P0)

**Duration:** 2-3 weeks
**Goal:** Single-PR takeover — focus the daemon on driving one PR to merge

### 3.1 Architecture (from Go)

The takeover feature allows an operator to say "focus all resources on this one PR and don't stop until it's merged." It:

1. **Claims a PR** — marks it as "taken over" in the daemon
2. **Runs a focused loop** — repeatedly runs Planner → Reviewer → Fixer → Worker cycle on that specific PR
3. **Handles failures** — retries, escalates, reports progress
4. **Can be stopped** — `looper takeover stop <repo>` or `--all`

### 3.2 Implementation Plan

#### 3.2.1 Domain Types (`looper-types`)

```rust
// New file: crates/looper-types/src/takeover.rs

pub struct TakeoverSession {
    pub id: String,
    pub project_name: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub pr_number: u64,
    pub status: TakeoverStatus,
    pub started_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub cycles_completed: u32,
    pub current_phase: TakeoverPhase,
    pub error_count: u32,
    pub max_errors: u32,
}

pub enum TakeoverStatus {
    Active,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

pub enum TakeoverPhase {
    Planning,
    Reviewing,
    Fixing,
    Working,
    Merging,
    Waiting,
}
```

#### 3.2.2 Storage (`looper-storage`)

```sql
-- Migration V6: takeover sessions
CREATE TABLE takeover_sessions (
    id TEXT PRIMARY KEY,
    project_name TEXT NOT NULL,
    repo_owner TEXT NOT NULL,
    repo_name TEXT NOT NULL,
    pr_number INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    started_at TEXT NOT NULL,
    last_activity TEXT NOT NULL,
    cycles_completed INTEGER NOT NULL DEFAULT 0,
    current_phase TEXT NOT NULL DEFAULT 'planning',
    error_count INTEGER NOT NULL DEFAULT 0,
    max_errors INTEGER NOT NULL DEFAULT 5,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

New repository: `TakeoverSessionsRepository` with CRUD + active session lookup.

#### 3.2.3 Runner (`looper-runner`)

```rust
// New file: crates/looper-runner/src/takeover.rs

pub struct TakeoverRunner {
    session: TakeoverSession,
    repos: Arc<SendRepos>,
    gateway: Arc<dyn Gateway>,
    executor: Arc<ConfiguredExecutor>,
    config: SchedulerConfig,
}

impl TakeoverRunner {
    /// Main takeover loop — runs until completion/failure/cancellation
    pub async fn run(&mut self) -> Result<TakeoverResult> {
        loop {
            match self.session.status {
                TakeoverStatus::Active => {
                    self.execute_cycle().await?;
                    self.session.cycles_completed += 1;
                    self.update_activity().await?;

                    // Check completion conditions
                    if self.is_pr_merged().await? {
                        self.session.status = TakeoverStatus::Completed;
                        break;
                    }
                    if self.session.error_count >= self.session.max_errors {
                        self.session.status = TakeoverStatus::Failed;
                        break;
                    }
                }
                TakeoverStatus::Paused => {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
                TakeoverStatus::Cancelled => break,
                _ => break,
            }
        }
        Ok(TakeoverResult::from(self.session))
    }

    /// Execute one full cycle: Review → Fix → Check → Repeat
    async fn execute_cycle(&mut self) -> Result<()> {
        // Phase 1: Review the PR
        self.session.current_phase = TakeoverPhase::Reviewing;
        let review_result = self.review_pr().await;

        // Phase 2: Fix any issues found
        if let Some(issues) = review_result.actionable_issues() {
            self.session.current_phase = TakeoverPhase::Fixing;
            self.fix_issues(issues).await?;
        }

        // Phase 3: Wait for CI
        self.session.current_phase = TakeoverPhase::Waiting;
        self.wait_for_checks().await?;

        // Phase 4: Handle merge
        self.session.current_phase = TakeoverPhase::Merging;
        self.try_merge().await?;

        Ok(())
    }
}
```

#### 3.2.4 CLI Commands (`looper-cli`)

```
# Start takeover
looper takeover <owner>/<repo>              # Take over a specific repo's PR
looper takeover <owner>/<repo> --pr 123     # Take over a specific PR
looper takeover <owner>/<repo> --max-errors 10

# Manage takeover
looper takeover list                        # List active takeovers
looper takeover status <owner>/<repo>       # Show takeover progress
looper takeover pause <owner>/<repo>        # Pause takeover
looper takeover resume <owner>/<repo>       # Resume paused takeover
looper takeover stop <owner>/<repo>         # Stop takeover
looper takeover stop --all                  # Stop all takeovers
```

#### 3.2.5 API Endpoints (`looper-api`)

```
POST   /api/projects/{name}/takeover              # Start takeover
GET    /api/projects/{name}/takeover              # Get takeover status
DELETE /api/projects/{name}/takeover              # Stop takeover
POST   /api/projects/{name}/takeover/pause        # Pause
POST   /api/projects/{name}/takeover/resume       # Resume
GET    /api/takeovers                              # List all active takeovers
```

#### 3.2.6 Skills

Port `skills/pr-takeover/SKILL.md` with both modes:
- **Live mode:** Uses `gh` + `git` in current session (for AI agents)
- **Background mode:** Hands off to `looper takeover` daemon

Include GraphQL recipes for:
- Review thread resolution
- Change request dismissal
- Reviewer re-request

---

## 4. Phase 2 — Web Dashboard (P0)

**Duration:** 3-4 weeks
**Goal:** React+Vite+TypeScript SPA served by looperd

### 4.1 Architecture

The dashboard is embedded into the `looperd` binary at build time. The React app is compiled to static assets, then included via `include_str!` or `rust-embed`.

```
crates/looperd/
├── dashboard/                    # React+Vite+TypeScript app
│   ├── src/
│   │   ├── App.tsx
│   │   ├── main.tsx
│   │   ├── pages/
│   │   │   ├── Overview.tsx      # Dashboard home
│   │   │   ├── Loops.tsx         # All loops with filters
│   │   │   ├── LoopDetail.tsx    # Single loop + runs
│   │   │   ├── Projects.tsx      # Project list + config
│   │   │   └── Config.tsx        # Config editor
│   │   ├── components/
│   │   │   ├── DataTable.tsx     # Reusable data table
│   │   │   ├── LoopActionBar.tsx # Start/stop/pause controls
│   │   │   ├── StatusChip.tsx    # Colored status badges
│   │   │   ├── ModelCombobox.tsx # Model selector
│   │   │   ├── PullRequestLink.tsx
│   │   │   ├── RecoveryCard.tsx  # Recovery actions
│   │   │   ├── ConfirmDialog.tsx
│   │   │   ├── CopyButton.tsx
│   │   │   └── ThemeToggle.tsx   # Dark/light mode
│   │   ├── lib/
│   │   │   ├── api.ts            # API client
│   │   │   ├── sse.ts            # SSE event streaming
│   │   │   ├── log-buffer.ts     # Log aggregation
│   │   │   ├── polling.ts        # Polling helpers
│   │   │   ├── config-form.ts    # Config form schema
│   │   │   ├── project-filter.ts
│   │   │   ├── worktree.ts       # Worktree helpers
│   │   │   └── theme.ts          # Theme management
│   │   └── types/
│   │       └── index.ts          # TypeScript types
│   ├── package.json
│   ├── vite.config.ts
│   ├── tsconfig.json
│   └── index.html
```

### 4.2 Build Integration

```rust
// crates/looperd/src/dashboard.rs

use rust_embed::Embed;

#[derive(Embed)]
#[folder = "dashboard/dist"]
struct DashboardAssets;

pub fn serve_dashboard() -> Router {
    Router::new()
        .nest_service("/", get(serve_index))
        .nest_service("/assets", get(serve_asset))
}

async fn serve_index() -> impl IntoResponse {
    let index = DashboardAssets::get("index.html").unwrap();
    Html(String::from_utf8(index.data.to_vec()).unwrap())
}

async fn serve_asset(Path(path): Path<String>) -> impl IntoResponse {
    match DashboardAssets::get(&path) {
        Some(content) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.to_string())], content.data.to_vec())
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
```

### 4.3 Pages Detail

#### Overview (Home)
- **Stats cards:** Total loops, active runs, success rate, avg duration
- **Recent activity feed:** Last 20 events (SSE-powered, live updates)
- **Quick actions:** Start loop, view projects, open config

#### Loops page
- **Filterable table:** Status, type (planner/reviewer/worker/fixer), project
- **Columns:** ID, Project, Type, Target, Status, Started, Duration, Runs
- **Actions:** Pause, Resume, Terminate (via LoopActionBar)
- **Auto-refresh:** SSE events trigger row updates

#### Loop Detail page
- **Header:** Loop info + status + actions
- **Runs timeline:** Chronological run list with status chips
- **Agent output:** Expandable panels showing agent stdout/stderr
- **PR links:** Clickable links to GitHub PRs
- **Recovery card:** Shows when loop is stuck, offers recovery actions

#### Projects page
- **Project list:** Name, repo path, base branch, provider
- **Add project:** Form with repo path, base branch
- **Project config:** Per-project agent config, role overrides
- **Sync button:** Trigger project sync

#### Config page
- **Config editor:** Form-based editing of all config sections
- **Validation:** Real-time validation with error/warning display
- **Environment variables:** Show LOOPER_* overrides
- **Save:** PUT to `/api/config` with validation

### 4.4 Dashboard API Endpoints

The existing API already has most endpoints needed. Add:

```
GET    /api/dashboard/overview    # Stats aggregate
GET    /api/dashboard/activity    # Recent events (last N)
```

### 4.5 Development Workflow

```bash
# Development
cd crates/looperd/dashboard
pnpm install
pnpm dev          # Vite dev server on :5173, proxies to looperd :7391

# Build (production)
pnpm build        # Outputs to dist/
cargo build -p looperd  # Embeds dashboard/dist/ into binary
```

---

## 5. Phase 3 — Forgejo/Gitea Provider (P1)

**Duration:** 2-3 weeks
**Goal:** Abstract forge interface, implement Forgejo/Gitea alongside GitHub

### 5.1 Forge Abstraction Layer

```rust
// New file: crates/looper-forge/src/lib.rs

#[async_trait]
pub trait Forge: Send + Sync {
    // Repository
    async fn get_repository(&self, owner: &str, name: &str) -> Result<Repository>;
    async fn get_repository_permission(&self, owner: &str, name: &str) -> Result<Permission>;

    // Issues
    async fn list_issues(&self, owner: &str, name: &str, filter: IssueFilter) -> Result<Vec<Issue>>;
    async fn get_issue(&self, owner: &str, name: &str, number: u64) -> Result<Issue>;
    async fn create_comment(&self, owner: &str, name: &str, number: u64, body: &str) -> Result<Comment>;

    // Pull Requests
    async fn list_pull_requests(&self, owner: &str, name: &str, filter: PrFilter) -> Result<Vec<PullRequest>>;
    async fn get_pull_request(&self, owner: &str, name: &str, number: u64) -> Result<PullRequest>;
    async fn create_pull_request(&self, owner: &str, name: &str, input: CreatePr) -> Result<PullRequest>;
    async fn merge_pull_request(&self, owner: &str, name: &str, number: u64, strategy: MergeStrategy) -> Result<()>;

    // Reviews
    async fn submit_review(&self, owner: &str, name: &str, pr: u64, input: ReviewInput) -> Result<()>;
    async fn list_review_threads(&self, owner: &str, name: &str, pr: u64) -> Result<Vec<ReviewThread>>;
    async fn resolve_review_thread(&self, owner: &str, name: &str, thread_id: &str) -> Result<()>;

    // Labels
    async fn add_label(&self, owner: &str, name: &str, issue: u64, label: &str) -> Result<()>;
    async fn remove_label(&self, owner: &str, name: &str, issue: u64, label: &str) -> Result<()>;

    // Webhooks
    async fn create_webhook(&self, owner: &str, name: &str, config: WebhookConfig) -> Result<Webhook>;
    async fn delete_webhook(&self, owner: &str, name: &str, id: u64) -> Result<()>;
    async fn list_webhooks(&self, owner: &str, name: &str) -> Result<Vec<Webhook>>;
}
```

### 5.2 Provider Implementations

| Provider | Method | Limitations |
|----------|--------|-------------|
| `GitHubForge` | `gh` CLI (existing) | Full support |
| `ForgejoForge` | REST API | No Coordinator, no auto-merge, no routed network, no webhooks |
| `GiteaForge` | REST API | Similar to Forgejo |

### 5.3 Config

```toml
[[projects]]
id = "my-project"
name = "my-project"
repoPath = "/path/to/repo"
baseBranch = "main"
provider = "forgejo"              # "github" (default), "forgejo", "gitea"

[projects.provider]
type = "forgejo"
url = "https://git.example.com"
token = "${FORGEJO_TOKEN}"
```

### 5.4 Files to Create

| File | Purpose |
|------|---------|
| `crates/looper-forge/src/lib.rs` | `Forge` trait definition |
| `crates/looper-forge/src/github.rs` | GitHub implementation (wraps existing `looper-github`) |
| `crates/looper-forge/src/forgejo.rs` | Forgejo REST API implementation |
| `crates/looper-forge/src/gitea.rs` | Gitea REST API implementation |
| `crates/looper-forge/src/types.rs` | Shared types (Repository, Issue, PullRequest, etc.) |
| `crates/looper-forge/src/errors.rs` | Forge error types |
| `Cargo.toml` changes | Add `looper-forge` to workspace |

### 5.5 Migration Path

1. Define `Forge` trait
2. Wrap existing `looper-github` as `GitHubForge`
3. Refactor all gateway calls to go through `Forge` trait
4. Implement `ForgejoForge`
5. Implement `GiteaForge`
6. Config routing: `project.provider` → `Forge` implementation

---

## 6. Phase 4 — HITL Workflows (P1)

**Duration:** 1-2 weeks
**Goal:** Human-in-the-loop decision points for complex scenarios

### 6.1 What HITL Does

When the daemon encounters a situation it can't resolve autonomously (merge conflict, failing CI after multiple fix attempts, ambiguous reviewer feedback), it:

1. **Pauses** the loop
2. **Sends a notification** (Slack, email, desktop, webhook)
3. **Waits for human input** via:
   - GitHub issue comment (e.g., `/looper retry`, `/looper merge`, `/looper skip`)
   - Slack message
   - Web dashboard action button
   - HITL inbox (Feishu/custom)

### 6.2 Implementation Plan

#### 6.2.1 HITL Decision Points

```rust
// crates/looper-runner/src/hitl.rs

pub enum HitlDecision {
    Retry,
    Skip,
    Merge,
    Abort,
    Custom(String),
}

pub struct HitlRequest {
    pub run_id: String,
    pub loop_id: String,
    pub project_name: String,
    pub reason: HitlReason,
    pub context: serde_json::Value,  // PR link, error details, etc.
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

pub enum HitlReason {
    MergeConflict,
    FailingCi,
    AmbiguousFeedback,
    ManualIntervention,
    PolicyViolation,
}

#[async_trait]
pub trait HitlTransport: Send + Sync {
    async fn request_decision(&self, request: &HitlRequest) -> Result<HitlDecision>;
    async fn check_response(&self, request_id: &str) -> Result<Option<HitlDecision>>;
}
```

#### 6.2.2 Transport Implementations

| Transport | Implementation |
|-----------|---------------|
| GitHub Comment | Listen for `/looper` slash commands on the PR |
| Slack | Post message with action buttons, listen for replies |
| Webhook | POST to configured URL, wait for callback |
| Dashboard | SSE event → UI button → POST decision |

#### 6.2.3 Config

```toml
[hitl]
enabled = true
defaultTransport = "github-comment"
timeoutMinutes = 60
maxRetries = 3

[hitl.transports.github-comment]
enabled = true

[hitl.transports.slack]
enabled = false
webhookUrl = ""

[hitl.transports.webhook]
enabled = false
callbackUrl = ""
```

---

## 7. Phase 5 — Advanced PR Management (P1)

**Duration:** 1-2 weeks
**Goal:** Auto-merge strategies, change-request dismissal, PR inspection commands

### 7.1 Auto-Merge Strategy Selection

Already partially implemented in `reviewer_criteria.rs`. Enhance:

```toml
[roles.reviewer.autoMerge]
enabled = true
strategy = "squash"    # "squash", "merge", "rebase"
scope = "all"          # "all", "spec-prs-only"
maxAge = "7d"          # Don't auto-merge PRs older than this
requireChecks = true   # Wait for CI to pass
requireApprovals = 0   # Minimum approvals needed
```

### 7.2 Change Request Dismissal

When reviewer approves but a previous change-request still exists:

```rust
// In reviewer.rs
async fn dismiss_stale_change_requests(&self, pr: &PullRequest) -> Result<()> {
    let reviews = self.gateway.list_reviews(owner, name, pr.number).await?;
    for review in reviews {
        if review.state == ReviewState::ChangesRequested && review.is_stale() {
            self.gateway.dismiss_review(owner, name, review.id, "Stale review dismissed").await?;
        }
    }
    Ok(())
}
```

### 7.3 PR Inspection Commands

```bash
looper pr list                              # List all PRs across projects
looper pr list --project <name>             # Filter by project
looper pr list --status open                # Filter by status
looper pr show <owner>/<repo>#123           # Detailed PR info
looper pr status <owner>/<repo>             # PR status summary
looper pr merge <owner>/<repo>#123          # Manual merge
looper pr close <owner>/<repo>#123          # Close PR
```

### 7.4 `logs-follow` Command

```bash
looper logs <run-id> --follow              # Stream logs in real-time
looper logs <run-id> --follow --filter "error"  # Filtered
```

Implementation: Connect to SSE endpoint, filter by run_id, stream to terminal.

### 7.5 `run-stats` Command

```bash
looper run-stats                           # Overall stats
looper run-stats --project <name>          # Per-project stats
looper run-stats --period 7d               # Last 7 days
```

Output:
```
Run Statistics (last 7 days)
─────────────────────────────
Total runs:        142
Success rate:      87.3%
Avg duration:      4m 23s
P95 duration:      12m 08s

By role:
  Planner:    38 runs, 92.1% success, avg 2m 15s
  Reviewer:   45 runs, 88.9% success, avg 3m 42s
  Worker:     32 runs, 81.3% success, avg 6m 18s
  Fixer:      27 runs, 85.2% success, avg 4m 55s
```

---

## 8. Phase 6 — Agent Skills & CLI Polish (P2)

**Duration:** 1 week
**Goal:** Complete CLI surface, agent skill files

### 8.1 Implement Hidden Stubs

| Command | Implementation |
|---------|---------------|
| `webhook status` | Show webhook forwarding stats |
| `webhook cleanup <repo>` | Remove orphan webhooks |
| `webhook rotate <repo>` | Rotate webhook secret |
| `webhook list-orphans` | List webhooks without matching project |
| `webhook delete <repo> --confirm` | Delete webhook |
| `labels` | Initialize/sync looper labels across projects |
| `prompt` | View/edit per-role agent prompts |
| `feedback` | Submit agent output feedback |
| `netadmin` | Network admin (list nodes, kick, status) |

### 8.2 CLI Polish

| Task | Details |
|------|---------|
| Consistent `--json` output | All commands support `--json` for machine parsing |
| Colored output | Use `colored` crate consistently across all commands |
| Progress indicators | Progress bars for long operations (using `indicatif`) |
| `--verbose` / `-v` flag | Toggle verbose output on all commands |
| Shell completions | Generate bash/zsh/fish completions |
| Man pages | Generate man pages for all commands |

---

## 9. Phase 7 — CI/CD & Testing Hardening (P2)

**Duration:** 1-2 weeks
**Goal:** Parity with Go's CI/CD, comprehensive E2E coverage

### 9.1 Sandbox E2E Workflow

Port from Go's `sandbox-e2e.yml`:

```yaml
# .github/workflows/sandbox-e2e.yml
name: Sandbox E2E
on:
  push:
    branches: [main]
  workflow_dispatch:

jobs:
  sandbox-e2e:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Build
        run: cargo build --release
      - name: Setup test repo
        run: scripts/setup-sandbox-repo.sh
      - name: Run sandbox E2E
        env:
          GITHUB_TOKEN: ${{ secrets.SANDBOX_TOKEN }}
          GITHUB_REPO: ${{ secrets.SANDBOX_REPO }}
        run: cargo test -p looper-e2e --features sandbox
      - name: Cleanup
        if: always()
        run: scripts/cleanup-sandbox.sh
```

### 9.2 E2E Test Scenarios to Add

| Scenario | What it tests |
|----------|---------------|
| Full planner→reviewer→worker→fixer cycle | End-to-end loop completion |
| Takeover flow | Start takeover → cycles → merge → cleanup |
| Daemon restart recovery | Start runs → kill daemon → restart → verify recovery |
| Webhook forwarding | Send webhook → verify queue items created |
| Multi-project | Two projects running concurrently |
| Agent vendor switch | Switch vendor mid-loop |
| Config hot-reload | Change config → verify daemon picks up changes |
| Dashboard smoke test | Start daemon → verify dashboard serves |

### 9.3 CI Pipeline Enhancements

| Enhancement | Details |
|-------------|---------|
| Coverage reporting | `cargo-tarpaulin` with coverage badge |
| Property-based testing | `proptest` for state machine transitions |
| Fuzzing | `cargo-fuzz` for config parser, completion marker parser |
| Benchmarks | `criterion` for critical paths (queue claim, config load) |

---

## 10. Phase 8 — Extras Beyond Go

**Duration:** Ongoing
**Goal:** Leverage Rust strengths to exceed Go original

### 10.1 Plane Provider Integration

```toml
[projects.provider]
type = "plane"
url = "https://plane.example.com"
apiKey = "${PLANE_API_KEY}"
workspace = "my-workspace"
```

Sync issues from Plane, create PRs for Plane tasks.

### 10.2 Advanced Metrics & Observability

```toml
[metrics]
enabled = true
backend = "prometheus"   # "prometheus", "datadog", "otlp"

[metrics.export]
endpoint = "http://localhost:9090"
interval = "30s"
```

### 10.3 Plugin System

```rust
// crates/looper-plugin/src/lib.rs

pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;

    async fn on_loop_start(&self, ctx: &LoopContext) -> Result<()>;
    async fn on_loop_complete(&self, ctx: &LoopContext, result: &LoopResult) -> Result<()>;
    async fn on_run_complete(&self, ctx: &RunContext, result: &RunResult) -> Result<()>;
    async fn on_error(&self, ctx: &ErrorContext, error: &Error) -> Result<()>;
}
```

### 10.4 Notification Rich Content

- Slack: Rich cards with PR links, run duration, action buttons
- Email: HTML templates with run summaries
- Desktop: Notification center with expandable details

---

## 11. Dependency Map

```
Phase 0 (Documentation) ──────────────────────────→ No deps
Phase 1 (Takeover)      ──────────────────────────→ No deps
Phase 2 (Dashboard)     ──────────────────────────→ Phase 0 (docs)
Phase 3 (Forgejo)       ──────────────────────────→ Phase 0 (docs)
Phase 4 (HITL)          ──────────────────────────→ Phase 1 (takeover uses HITL)
Phase 5 (PR Management) ──────────────────────────→ Phase 3 (forge abstraction)
Phase 6 (CLI Polish)    ──────────────────────────→ Phase 1, 5
Phase 7 (CI/CD)         ──────────────────────────→ Phase 1, 2
Phase 8 (Extras)        ──────────────────────────→ All above
```

### Parallel Execution Opportunities

- **Phase 0 + Phase 1** can run in parallel
- **Phase 2 + Phase 3** can run in parallel (after Phase 0)
- **Phase 4 + Phase 5** can run in parallel (after Phase 1 + 3)
- **Phase 6 + Phase 7** can run in parallel (after Phase 1 + 5)

---

## 12. Effort Estimates

| Phase | Duration | Dependencies | Risk |
|-------|----------|-------------|------|
| Phase 0 — Quick Wins | 2-3 days | None | Low |
| Phase 1 — Takeover | 2-3 weeks | None | Medium |
| Phase 2 — Dashboard | 3-4 weeks | Phase 0 | Medium |
| Phase 3 — Forgejo | 2-3 weeks | Phase 0 | Medium |
| Phase 4 — HITL | 1-2 weeks | Phase 1 | Low |
| Phase 5 — PR Management | 1-2 weeks | Phase 3 | Low |
| Phase 6 — CLI Polish | 1 week | Phase 1, 5 | Low |
| Phase 7 — CI/CD | 1-2 weeks | Phase 1, 2 | Low |
| Phase 8 — Extras | Ongoing | All | High |
| **Total (excluding Phase 8)** | **~10-14 weeks** | | |

### Critical Path

```
Phase 0 (3d) → Phase 1 (2w) → Phase 4 (1w) → Phase 8 (ongoing)
            ↘                    ↗
Phase 2 (3w) → Phase 7 (1w)  →
            ↘                    ↗
Phase 3 (2w) → Phase 5 (1w) → Phase 6 (1w)
```

**Estimated total to feature parity: 10-14 weeks (2.5-3.5 months)**

---

## Appendix: ADR Titles (to port from Go)

| # | Title |
|---|-------|
| 0001 | Coordinator is stateless (all state in GitHub labels) |
| 0002 | Coordinator authority via durable labels |
| 0004 | Dependency gate via GitHub native `blocked_by` |
| 0005 | Auto-merge via GitHub native + Coordinator merge-watch |
| 0006 | Webhook tunnel mode |
| 0007 | Coordinator admission/assignment authority |
| 0008 | Network mode as role reactivity authority |
| 0009 | Network mode compact target labels |
| 0010 | GitHub labels as work eligibility authority |
| 0011 | Coordinator control plane for routed projects v1 |
| 0012 | SQLite project authority |
| 0013 | Local operator dashboard |
| 0014 | Config file is global runtime policy authority |
| 0015 | Execution supervisor live ownership |
| 0016 | Agent model catalog is advisory |

---

*This plan is a living document. Update as implementation progresses.*
