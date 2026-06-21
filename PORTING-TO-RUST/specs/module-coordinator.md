# Module: looper-coordinator — Rust Spec

> Derived from `internal/coordinator/runner.go` (2200+ dong), `internal/coordinator/triage/triage.go`, `internal/coordinator/dispatch/dispatch.go`, `internal/coordinator/depgraph/depgraph.go`, `internal/coordinator/mergewatch/mergewatch.go`

Co-ordinator là tick-based orchestrator, khong phai step-based runner nhu Planner/Reviewer/Fixer/Worker. No chay trong vong lap discover cua Scheduler: moi tick se goi `DiscoverIssues()` de thuc hien triage, dispatch, dependency management va mergewatch cho toan bo open issues cua mot project.

---

## 1. TRIAGE

### 1.1 ShouldTriage — Guard nhap cuoc

```rust
fn should_triage(issue: &Issue, cfg: &TriageConfig, now: DateTime<Utc>) -> bool {
    // Neu issue da co triaged_label -> false
    if has_label(&issue.labels, &cfg.triaged_label) {
        return false;
    }
    // Parse issue.created_at
    // Neu issue qua cu (MaxIssueAgeDays) -> false
    // Nguoc lai -> true
}
```

**Quy tac:**
- Chi triage issue chua co `triaged_label`
- Bo qua issue co `created_at` qua xa (mac dinh: 365 ngay)
- Neu khong parse duoc `created_at` -> fail closed (false)

### 1.2 ShouldReTriage — Guard triage lai

```rust
fn should_retriage(issue: &Issue, cfg: &TriageConfig) -> bool {
    // Chi active khi ReTriageOnAuthorReply = true
    // Chi ap dung cho issue co unclear_label (vi du "needs-info")
    // Tim lan cuoi cung unclear_label duoc ap dung (qua timeline event "labeled")
    // Neu co comment tu author sau thoi diem do -> true (can retriage)
    // Nguoc lai -> false
}
```

**Muc dich:** Khi issue bi danh "needs-info" va tac gia da tra loi, coordinator can triage lai de cap nhat phan loai.

### 1.3 Triage Config

```rust
struct TriageConfig {
    triaged_label:            String,        // "looper:triaged"
    max_issue_age_days:       u32,           // bo qua issue qua cu
    max_per_tick:             u32,           // gioi han so luong triage / tick
    out_of_scope_label:       String,        // "looper:wontfix"
    unclear_label:            String,        // "looper:needs-info"
    retriage_on_author_reply: bool,          // tu dong retriage khi author reply
}
```

### 1.4 LLM Decision — triage.Decide()

**Input:** `TriageInput` gom Issue + RepoContext + Config + Now

**Prompt** duoc xay dung boi `build_prompt()`:
```
You are Looper Coordinator triage. Return strict JSON only.
Allowed dispositions: valid, out-of-scope, unclear.
Allowed kind labels: kind/bug, kind/feature, kind/docs, kind/refactor
Allowed area labels: area/api, area/config, area/coordinator, area/docs, area/github, area/runtime, area/testing, area/planner, area/worker, area/reviewer
Allowed complexity labels: complexity/s, complexity/m, complexity/l
Allowed dispatch labels: dispatch/plan, dispatch/implement

Output schema:
{"disposition":"valid|out-of-scope|unclear","comment":"string","labels":{"kind":["kind/..."],"area":["area/..."],"complexity":["complexity/..."],"dispatch":["dispatch/..."]}}

Issue: {title}
{body}
[RepoContext paths & symbols]
```

**LLM Output Schema** (tu LLM raw JSON):

```json
{
  "disposition": "valid",
  "comment": "This is a bug in the coordinator that affects dispatch logic.",
  "labels": {
    "kind": ["kind/bug"],
    "area": ["area/coordinator"],
    "complexity": ["complexity/m"],
    "dispatch": ["dispatch/plan"]
  }
}
```

**Internal Decision struct** (sau khi parse va validate):

```rust
struct Decision {
    no_op:                bool,
    disposition:          Disposition,   // Valid | OutOfScope | Unclear
    clear_label_patterns: Vec<String>,   // mac dinh: ["kind/*", "area/*", "complexity/*", "dispatch/*", out_of_scope_label, unclear_label]
    remove_labels:        Vec<String>,
    apply_labels:         Vec<String>,
    comment_body:         String,
    mark_triaged:         bool,
}
```

### 1.5 Parse & Validation Logic (`parse_decision`)

| Disposition | Yeu cau Labels | ApplyLabels | ClearLabelPatterns |
|-------------|---------------|-------------|-------------------|
| `valid` | Bat buoc: 1 kind, 1 area, 1 complexity, 1 dispatch, tat ca trong allow list | [kind, area, complexity, dispatch, triaged_label] | Pattern mac dinh |
| `out-of-scope` | Khong duoc co label nao | [out_of_scope_label, triaged_label] | Pattern mac dinh |
| `unclear` | Khong duoc co label nao | [unclear_label, triaged_label] | Pattern mac dinh |

- Neu parse fail (JSON invalid, missing comment, disposition unknown, label khong hop le) -> `NoOpDecision()` (fail closed)
- `require_exactly_one()`: validate moi category co dung 1 label va nam trong allowed list

### 1.6 Decision Application (`apply_decision`)

Trinh tu ap dung decision len GitHub Issue:

1. **Remove triaged label** neu `mark_triaged == true` va issue da co triaged_label (re-apply)
2. **Clear label patterns** (xoa label theo `clear_label_patterns`)
   - `split_delayed_label_patterns`: phan tach pattern can xoa ngay va pattern cho sau khi comment duoc post
   - `unclear_label` va pattern khac bi delayed neu issue chua co triaged_label (de tranh xoa label truoc khi comment duoc dang)
3. **Remove labels** (xoa label cu the theo `remove_labels`)
   - Cung co split delayed logic nhu tren
4. **Apply labels** (them label moi), ngoai tru triaged_label (se xu ly rieng)
5. **Post comment** (neu `comment_body` khong rong) — dung `post_or_edit_comment()` de giam spam: update comment cu neu da co
6. **Mark triaged** — them `triaged_label` neu issue chua co
7. **Sau khi comment thanh cong**: xoa not cac label bi delayed

### 1.7 Reactions (dung chung voi Dispatch)

Coordinator co the them reaction vao comment qua:
```rust
fn add_issue_reaction(repo, comment_id, content) -> Result
```
`content`: "+1" (thanh cong) hoac "confused" (that bai)

### 1.8 Gioi han: MaxPerTick va che do dong

```rust
fn limit_per_tick<T>(items: Vec<T>, max: u32) -> Vec<T> {
    // Neu max <= 0 hoac len(items) <= max -> tra ve copy
    // Nguoc lai -> chi lay max phan tu dau
}
```

O runner: duyet loaded issues, bo qua nhung issue thuoc `merge_watch_retriggers` hoac `retriage_issue_numbers`, dem `processed`, dung khi `processed >= triage_cfg.max_per_tick`.

---

## 2. DISPATCH

### 2.1 dispatch.Decide() — hai che do

```rust
fn decide(
    issue: &DispatchIssue,
    cfg: &DispatchConfig,
    now: DateTime<Utc>,
    graph: Option<&DependencyGraph>,
) -> DispatchAction
```

| Che do | Gia tri `cfg.mode` | Behavior |
|--------|-------------------|----------|
| Human-gated | `"human-gated"` | Cho slash command (`/plan`, `/implement`) |
| Autonomous | `"autonomous"` | Tu dong dispatch sau mot khoang delay |

### 2.2 Dispatch Config

```rust
struct DispatchConfig {
    mode:                  DispatchMode,    // HumanGated | Autonomous
    triaged_label:         String,          // "looper:triaged"
    hold_label:            Option<String>,  // "looper:hold"
    autonomous_delay:      Duration,        // 30 phut
    allowed_users:         Vec<String>,     // empty = moi nguoi co write access
    slash_commands:        Vec<String>,     // ["/plan", "/implement"]
    assign_to:             Option<String>,  // "octocat"
    planner_trigger_labels: Vec<String>,    // ["looper:plan"]
    worker_trigger_labels:  Vec<String>,    // ["looper:worker-ready"]
}
```

### 2.3 Dispatch Issue Model

```rust
struct DispatchIssue {
    number:     i64,
    labels:     Vec<String>,
    comments:   Vec<Comment>,
    triaged_at: Option<DateTime<Utc>>,
}
```

### 2.4 Dispatch Action (Output)

```rust
struct DispatchAction {
    no_op:                bool,
    trigger_labels:       Vec<String>,     // labels can them de kick hoat runner
    assign_to:            Option<String>,  // nguoi duoc assign
    reaction_comment_id:  i64,             // ID cua slash command comment de reaction
    reaction_content:     String,          // "+1" hoac "confused"
    failure_comment_body: Option<String>,  // comment bao loi (neu co)
}
```

### 2.5 Slash Command Parsing

```rust
fn parse_slash_command(body: &str, configured: &[String]) -> Option<&str>
```

**Quy tac parse:**
- Duyet tung dong cua comment body
- Bo qua code fences: dong bat dau bang ``` hoac ~~~ -> toggle trang thai in_fence
- Bo qua dong trong code fence
- Bo qua blockquote: dong bat dau bang `>`
- Chi nhan command o dau dong (co the co whitespace truoc)
- Command phai co word boundary sau do (space, tab, hoac het dong)
- Vi du hop le: `/plan`, `  /implement`, `/plan some context` -> nhan `/plan`
- Vi du khong hop le: `please /plan this` (khong o dau dong), `/planner` (khong khop command)

**Ham ho tro:**
```rust
fn configured_commands(configured: &[String]) -> HashMap<String, bool>
fn command_boundary(value: &str, index: usize) -> bool
fn command_dispatch_label(command: &str) -> Option<&str>  // "/plan" -> "dispatch/plan"
```

### 2.6 Human Gate

**Luong xu ly `decide_human_gated()`:**

1. **Tim slash command** (duyet comment tu moi nhat -> cu nhat)
   - `latest_command_attempt()`: tim comment co slash command hop le tu allowed user
   - Bo qua comment tu user khong co quyen
   - Neu khong tim thay -> `Action::no_op()`
2. **Kiem tra triaged**: neu issue chua co `triaged_label` -> fail (confused reaction + failure comment)
3. **Kiem tra dispatch label**: triage phai set dung 1 dispatch label (`dispatch/plan` hoac `dispatch/implement`)
   - Neu nhieu hon 1 -> fail (ambiguous)
   - Neu khong khop voi slash command -> fail
4. **Kiem tra trigger labels**: neu trigger label da co -> `Action::no_op()` + success reaction (idempotent)
5. **Kiem tra dependency gate**: neu dependency graph co blocker unsatisfied -> fail (confused reaction + failure comment liet ke blockers)
6. **Thanh cong**: tra ve `Action` voi `trigger_labels`, `assign_to`, `+1 reaction`

**Permission check (`is_allowed_user`):**

```rust
fn is_allowed_user(comment: &Comment, allowed_users: &[String]) -> bool {
    // Neu allowed_users trong -> chi can co write access
    // Neu comment.author nam trong allowed_users -> true
    // Neu comment.has_write_access -> true (fallback)
    // Nguoc lai -> false
}
```

**Reaction handling:**
- Success: reaction `+1` vao slash command comment
- Failure: reaction `confused` + post failure comment (co marker `<!-- looper:coordinator:dispatch-failure -->`)

### 2.7 Autonomous Mode

**Luong xu ly `decide_autonomous()`:**

1. Neu chua co `triaged_label` -> `Action::no_op()`
2. Neu khong co dispatch label duy nhat -> `Action::no_op()`
3. Neu khong co trigger labels -> `Action::no_op()`
4. **Hold label veto**: neu co hold_label -> `Action::no_op()`
5. **Trigger already present veto**: neu trigger labels da duoc ap dung -> `Action::no_op()`
6. **Delay gate**: neu `triaged_at` khong co hoac `now < triaged_at + autonomous_delay` -> `Action::no_op()`
7. **Dependency gate**: neu `graph.unsatisfied()` khac rong -> `Action::no_op()` (khong post failure comment, chi im lang cho tick sau)
8. **Thanh cong**: tra ve `Action` voi `trigger_labels` + `assign_to`

### 2.8 Label Logic Helpers

```rust
fn single_dispatch_label(labels: &[String]) -> Option<String>
    // Tim label bat dau bang "dispatch/"
    // Neu 0 hoac > 1 -> None (ambiguous)
    // Neu dung 1 -> Some(label)

fn trigger_labels_for_dispatch(dispatch_label: &str, cfg: &DispatchConfig) -> Vec<String>
    // "dispatch/plan" => cfg.planner_trigger_labels
    // "dispatch/implement" => cfg.worker_trigger_labels

fn missing_labels(existing: &[String], want: &[String]) -> Vec<String>
    // Tra ve label trong want ma khong co trong existing
```

### 2.9 Dependency Gate

```rust
fn needs_dependency_gate(issue: &DispatchIssue, cfg: &DispatchConfig, now: DateTime<Utc>) -> bool
    // Human-gated: co slash command, da triaged, dispatch label khop, trigger labels con thieu
    // Autonomous: da triaged, co dispatch label, khong hold, da qua delay, trigger labels con thieu
```

Dependency gate duoc kiem tra o runner truoc khi build dependency graph:
```rust
fn dispatch_dependency_candidates(loaded, cfg, now) -> Vec<i64>
    // Loc nhung issue can dependency gate
```

**Unsatisfied blocker handling:**
- Human-gated: post failure comment liet ke blocker + confused reaction
- Autonomous: silent no-op (cho tick sau)

### 2.10 Edge Cases

| Tinh huong | Human-gated | Autonomous |
|------------|-------------|------------|
| Nhieu `dispatch/` labels | Fail (ambiguous) | NoOp |
| Khong co `dispatch/` label | Fail (missing triage) | NoOp |
| Slash command tu user khong co quyen | Ignore, tim comment cu hon | N/A |
| Hold label set | N/A | NoOp |
| Delay chua het | N/A | NoOp |
| Trigger labels da co | NoOp + success reaction | NoOp |
| Blocker unsatisfied | Fail + failure comment | NoOp (cho tick sau) |
| Issue chua triaged | Fail + failure comment | NoOp |

---

## 3. DEPGRAPH (Dependency Graph)

### 3.1 Core Types

```rust
struct IssueRef {
    repo:   String,    // "owner/repo"
    number: i64,
}

struct IssueState {
    state:        String,   // "open" | "closed"
    state_reason: String,   // "completed" | "not_planned" | "duplicate"
}

struct Snapshot {
    blocked_by:   HashMap<IssueRef, Vec<IssueRef>>,
    issues:       HashMap<IssueRef, IssueState>,
    unreachable:  Vec<IssueRef>,
}

struct Blocker {
    issue:             IssueRef,
    state:             String,
    state_reason:      String,
    satisfied:         bool,
    requires_retriage: bool,
    unreachable:       bool,

    // Flattened fields (populated by Unsatisfied() method)
    number:    i64,
    repo:      String,
    reachable: bool,
}

struct DependencyGraph {
    ready_set:    Vec<IssueRef>,
    cycles:       Vec<Cycle>,
    unreachable:  Vec<IssueRef>,
    blockers:     HashMap<IssueRef, Vec<Blocker>>,
}

type Cycle = Vec<IssueRef>;
```

### 3.2 BuildGraph (`Build`)

```rust
fn build(tracked: Vec<IssueRef>, snapshot: Snapshot) -> DependencyGraph
```

**Cac buoc:**

1. **Normalize input:** loai bo duplicate, bo qua IssueRef voi number <= 0

2. **Xay dung blocked_by map tu snapshot**

3. **Duyet tracked issues:**
   - Voi moi issue, lay blocked_by dependencies
   - Voi moi dependency:
     - `new_blocker()`: xac dinh blocker satisfied hay unsatisfied
     - Neu satisfied -> bo qua
     - Neu unsatisfied -> them vao blockers map
     - Neu unreachable (khong co state trong snapshot) -> them vao unreachable set
     - Neu tracked dependency (cung nam trong tracked list) -> them vao edges (cho cycle detection)

4. **Xac dinh ready_set:** issue khong co blocker unsatisfied -> ready

5. **Phat hien cycle:** goi `detect_cycles()` tren tracked + edges

### 3.3 Blocker State Classification

```rust
fn classify_blocker_state(state: &IssueState) -> BlockerDisposition
```

| State | StateReason | Satisfied | RequiresReTriage |
|-------|-------------|-----------|-----------------|
| `closed` | `completed` | `true` | `false` |
| `closed` | `not_planned` | `false` | `true` |
| `closed` | `duplicate` | `false` | `true` |
| `open` | *bat ky* | `false` | `false` |
| *khac* | *bat ky* | `false` | `false` |

**Giai thich:**
- `closed` + `completed`: blocker da duoc hoan thanh -> issue ready
- `closed` + `not_planned` / `duplicate`: blocker closed nhung khong phai completed -> can retriage issue de xem xet lai
- `open`: blocker con mo -> issue blocked
- Khong co state trong snapshot -> unreachable (blocker khong the xac dinh)

### 3.4 Cycle Detection

```rust
fn detect_cycles(tracked: Vec<IssueRef>, edges: HashMap<IssueRef, Vec<IssueRef>>) -> Vec<Cycle>
```

**Thuat toan:** DFS voi tracking stack (phat hien back edge)

1. Duyet tung node trong tracked order
2. Duyet DFS:
   - Danh dau node `visiting` (state = 1)
   - Push node vao stack + luu stack index
   - Voi moi neighbor trong edges:
     - Neu neighbor co trong stack (back edge) -> phat hien cycle
     - Neu neighbor chua tham -> DFS tiep
   - Pop node khoi stack, danh dau `visited` (state = 2)
3. **Canonicalize cycle:** xoay cycle de co dinh dang nhat quan (sort theo string compare)
4. **Deduplicate cycles** bang cycle key (vd: `#1->#2->#3->#1`)

**Vi du:**
- Two-node cycle: `[#1, #2, #1]`
- Three-node cycle: `[#1, #2, #3, #1]`
- Self-loop: `[#1, #1]`

### 3.5 Sub-issue Relationships

O runner, sau khi xay dung dependency state, coordinator xu ly sub-issue relationships qua `ListSubIssues()` API.

```rust
fn populate_parent_ordering(loaded, ready_set, state) -> HashMap<i64, IssueOrder>
```

**Muc dich:** Khi dispatch autonomous mode, coordinator sap xep autonomous dispatch candidates theo thu tu:
1. Sub-issues duoc dispatch truoc parent issue (theo index order)
2. Neu cung parent -> dispatch theo sub-issue index
3. Neu khong lien quan -> dispatch theo issue number

```rust
struct IssueOrder {
    parent_number: i64,
    index:         usize,
}
```

### 3.6 Dependency Actions (o runner)

`apply_dependency_actions()` xu ly cac issue can retriage do dependency:

1. Duyet `tracked` issues
2. Neu issue nam trong `retriage_issue_numbers`:
   - Xoa `triaged_label` + `dispatch/*` labels
   - Post cycle comment (neu co cycle)
3. **Cycle comment body**: liet ke cycle path: `"Dependency cycle detected: #1 -> #2 -> #3 -> #1"`

---

## 4. MERGEWATCH

### 4.1 8 Watch Action Types

| Action | Constant | Y nghia | Hanh dong tiep theo |
|--------|----------|---------|---------------------|
| `Merged` | `ActionMerged` | PR da duoc merge thanh cong | Trigger downstream (worktree cleanup, issue close) |
| `StillPending` | `ActionStillPending` | PR mergeable, CI passing, cho GitHub merge | Wait, tick tiep theo |
| `Indeterminate` | `ActionIndeterminate` | Mergeable state "unknown", GitHub dang tinh toan | Tiep tuc watch, track `first_unknown_at` |
| `Conflict` | `ActionConflict` | Mergeable state = "dirty", co merge conflict | Report conflict, stop watching |
| `RedCI` | `ActionRedCI` | Required checks failed | Report failure, stop watching |
| `BranchProtectionChanged` | `ActionBranchProtectionChanged` | Unknown qua lau, hoac missing required checks | Skip, free capacity |
| `HumanDisabledAutoMerge` | `ActionHumanDisabledAutoMerge` | Auto-merge bi human tat | Stop watching |
| `TransientError` | `ActionTransientError` | Temporary GitHub API error (502/503, timeout) | Retry voi backoff |

### 4.2 PRSnapshot Model

```rust
struct PRSnapshot {
    repo:                      String,
    pr_number:                 i64,
    issue_number:              i64,
    head_sha:                  String,
    merged:                    bool,
    open:                      bool,
    auto_merge_enabled:        bool,
    auto_merge_owned_by_looper: bool,
    has_looper_label:          bool,
    mergeable:                 Option<bool>,     // nil = unknown
    mergeable_state:           String,           // "clean" | "dirty" | "unknown" | "blocked" | "behind"
    required_checks:           RequiredCheckSummary,
    temporary_error:           Option<TemporaryError>,
}

struct RequiredCheckSummary {
    failed:  Vec<String>,   // CI checks that failed
    pending: Vec<String>,   // CI checks still running
    missing: Vec<String>,   // Expected checks not yet reported
}

struct TemporaryError {
    suggested_delay: Duration,   // e.g. 60s
}
```

### 4.3 PriorWatchMarker — Persistence giua cac tick

```rust
struct PriorWatchMarker {
    pr_number:       i64,
    head_sha:        String,
    retries:         usize,
    first_unknown_at: Option<DateTime<Utc>>,
    next_retry_at:   Option<DateTime<Utc>>,
}
```

**Reset logic:** Neu `prior` khong co, hoac `pr_number`/`head_sha` thay doi -> reset voi `fallback_retries`

### 4.4 Retry Budget

```rust
struct RetryBudget {
    now:                       DateTime<Utc>,
    transient_retries:         usize,    // max transient retries (mac dinh: 3)
    max_indeterminate_duration: Duration, // thoi gian toi da cho "unknown" (mac dinh: 15 phut)
}
```

### 4.5 Classification Algorithm (`Classify`)

```python
def classify(snapshot: PRSnapshot, prior: Option<PriorWatchMarker>, budget: RetryBudget) -> WatchAction:

    # Buoc 1: Normalize prior
    prior = normalize_prior(prior, snapshot, budget.transient_retries)

    # Buoc 2: Evaluate theo thu tu uu tien (first-match)

    # 2a. TemporaryError
    if snapshot.temporary_error is not None:
        retries_left = prior.retries
        if prior_preserved(prior, snapshot):
            retries_left = max(0, retries_left - 1)
        exhausted = (retries_left == 0)
        return Action(TransientError, retries_left, suggested_delay, exhausted)

    # 2b. Merged
    if snapshot.merged:
        return Action(Merged)

    # 2c. AutoMerge disabled (and was previously enabled)
    if prior exists and same PR and not snapshot.auto_merge_enabled:
        return Action(HumanDisabledAutoMerge)

    # 2d. Mergeable unknown
    if snapshot.mergeable is None or snapshot.mergeable_state == "unknown":
        first_unknown = prior.first_unknown_at or now
        if max_indeterminate_duration exceeded:
            return Action(BranchProtectionChanged, first_unknown, deadline_exceeded=true)
        else:
            return Action(Indeterminate, first_unknown)

    # 2e. Dirty (conflict)
    if snapshot.mergeable_state == "dirty":
        return Action(Conflict)

    # 2f. Required checks failed
    if snapshot.required_checks.failed is not empty:
        return Action(RedCI)

    # 2g. Missing required checks
    if snapshot.required_checks.missing is not empty:
        return Action(BranchProtectionChanged)

    # 2h. Default: still pending
    return Action(StillPending)
```

### 4.6 WatchAction Output

```rust
struct WatchAction {
    kind:              WatchActionKind,
    first_unknown_at:  Option<DateTime<Utc>>,
    deadline_exceeded: bool,
    retries_left:      usize,
    suggested_delay:   Duration,
    exhausted:         bool,
}
```

### 4.7 Edge Cases

| Tinh huong | Phan loai | Ly do |
|------------|-----------|-------|
| First tick, mergeable unknown, khong prior | `ActionIndeterminate` | Track `first_unknown_at`, retry tick sau |
| Unknown > 15 phut | `ActionBranchProtectionChanged` | Co the branch protection da thay doi |
| GitHub 502/503 | `ActionTransientError` | Retry voi backoff, giam `retries_left` |
| Head SHA thay doi giua cac tick | Reset prior | Fresh retry count |
| Auto-merge bi human disable | `ActionHumanDisabledAutoMerge` | Khong can watch nua |
| PR merged | `ActionMerged` | Trigger post-merge actions |
| Required check failed | `ActionRedCI` | Stop watching |
| Merge conflicts | `ActionConflict` | Stop watching |
| PR con pending, mergeable clean | `ActionStillPending` | Cho GitHub auto-merge |

### 4.8 Tich hop vao Coordinator Runner

Trong `apply_merge_watch()`:

1. Duyet loaded issues
2. Tim PRs linked voi issue (qua `ListLinkedPullRequests`)
3. Voi moi PR co auto-merge enabled:
   - Lay `PullRequestDetail` (mergeable, checks, state)
   - Xay dung `PRSnapshot`
   - Load `PriorWatchMarker` tu storage (theo key: `mergewatch:{repo}:{prNumber}`)
   - Goi `Classify()` de phan loai
   - Theo tung loai:
     - `Merged`: cleanup worktree, close issue, post comment
     - `StillPending`: cap nhat `PriorWatchMarker`, persist cho tick sau
     - `Indeterminate`: cap nhat `first_unknown_at`, persist
     - `Conflict`: post conflict comment, remove labels
     - `RedCI`: post failure comment, remove labels
     - `BranchProtectionChanged`: post warning, remove labels
     - `HumanDisabledAutoMerge`: remove looper labels, clean up
     - `TransientError`: persist voi retries giam dan
4. Khi retries exhausted -> final comment, remove labels, stop tracking

---

## 5. Coordinator Runner Flow (Tong hop)

```
DiscoverIssues(project_id, repo, snapshot):
  1. should_run_tick(): rate limit check (>= 2s giua cac tick)
  2. ListOpenIssues(repo, limit=100)
  3. For moi issue: loadIssue() -> detail, timeline, comments
  4. applyMergeWatch():
     - Retrigger downstream actions cho merged PRs
     - Tra ve set issue numbers can exclude khoi triage/dispatch
  5. filterLoadedIssues(): loai bo merge-watch issues
  6. buildDependencyState():
     - Build depgraph tu GitHub dependencies API
     - Phat hien cycles
     - Xac dinh retriage candidates (blocker not_planned/duplicate)
     - Xu ly sub-issue ordering
  7. applyDependencyActions():
     - Post cycle comments
     - Remove labels cho issue can retriage
  8. applyDispatches():
     - Human-gated: duyet tung issue, goi Decide() -> applyDispatchAction()
     - Autonomous: xep hang dispatch candidates (sub-issue order, budget), ap dung
  9. applyReviewAssignments():
     - Add reviewers to PRs dua tren config
  10. For moi issue (exclude merge-watch + retriage candidates):
      - Kiem tra ShouldTriage / ShouldReTriage
      - Neu can: decide() -> applyDecision()
      - Gioi han: MaxPerTick
  11. Tra ve DiscoveryResult{Ticked: true}
```

### 5.1 Rate Limit (`shouldRunTick`)

```rust
fn should_run_tick(project_id: &str) -> bool {
    // Luu lastTickByProject
    // Neu last_tick == None hoac now - last_tick >= 2s -> true, cap nhat last_tick
    // Nguoc lai -> false (Skipped)
}
```

### 5.2 Network Mode — Routed Worker Admission

Khi project o mode `routed`, controller co the dispatch worker den network node thay vi local:

1. Kiem tra `workerAdmissionIntent()`: neu action co worker trigger labels
2. Kiem tra `currentNodeHoldsLease()`: coordinator must hold active lease
3. `selectEligibleWorkerNode()`: chon node khoe nhat (dynamic load thap nhat)
4. `revalidateCoordinatorLease()`: dam bao lease con hieu luc
5. Ap dung assignee (GitHub login cua worker node) + trigger labels + target labels

### 5.3 Autonomous Dispatch Budget

O che do autonomous, coordinator gioi han so luong dispatch moi tick:

```rust
fn dispatch_budget(loaded, ready, downstream_labels) -> (budget, preempt_workers)
```

- `budget = max_concurrent_runs - running_count`
- Neu co worker dispatch va running+workers >= max -> kiem tra co reviewer/fixer work pending khong
- Neu co reviewer/fixer pending -> `preempt_workers = true` (uu tien reviewer/fixer hon worker)
