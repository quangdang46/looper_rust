# Module 7: looper-runner — Rust Spec

> Derived from planner/runner.go (2198), reviewer/runner.go (6184), fixer/runner.go (7114),
> worker/runner.go (3810), coordinator/runner.go (2096), failureclass/failureclass.go (179), specpr/specpr.go (72)

---

## 1. Planner State Machine

### Step Sequence
```
discover-issues → prepare-worktree → write-spec → publish → notify
```

### Step Enums
```
stepDiscoverIssues  = "discover-issues"
stepPrepareWorktree = "prepare-worktree"
stepWriteSpec       = "write-spec"
stepPublish         = "publish"
stepNotify          = "notify"
```

### 1.1 discover-issues
**GitHub API calls:**
- `ViewIssue(repo, issueNumber)` → issue detail
- `GetCurrentUserLogin(repo)` → current user (if needed)
- `AddIssueAssignees(repo, issue, assignee)` — auto-assign for manual queue

**Logic:**
1. Parse issue from queue payload or loop target
2. Fetch issue detail from GitHub
3. Try to acquire lock `issue:{repo}:{number}` (TTL: 10min)
4. Check labels match policy (ALL/ANY mode)
5. If manual queue, auto-assign current user
6. Build checkpoint with issue info, spec path, requested reviewers
7. If labels don't match or not assigned → set `SkipReason` and advance

**Errors:**
- Missing repo/issue → NonRetryable
- Lock held → RetryableTransient
- Login resolution failure → RetryableAfterResume
- Assignee failure → RetryableAfterResume

### 1.2 prepare-worktree
**GitHub/Git API calls:**
- `CreateWorktree(projectID, repoPath, branch, baseBranch)` → creates git worktree

**Logic:**
1. Validate existing worktree checkpoint (resume safety)
2. Build branch name: `looper/planner/{issueNumber}-{slug}`
3. Base branch: project config or "main"
4. Initialize lifecycle state

**Errors:**
- Worktree creation failure → generic error (classified by boundary)

### 1.3 write-spec
**Git/GitHub API calls:**
- `InspectHead()` — check for uncommitted changes
- `Commit()` — fallback commit if uncommitted

**Agent prompt (built by `buildPlannerPrompt`):**
```
Write a planning spec for GitHub issue {repo}#{number}.
Repository: {repo}
Base branch: {base}
Spec path: {specPath}
Issue title: {title}
Issue body: {body}
Issue URL: {url}
[AGENTS.md content]
[custom instructions]
Requirements:
- Create or update the spec at {specPath}
- Use Markdown with clear problem, goals, approach, risks, validation
- Commit changes or leave for Looper based on allowAutoPush
[lifecycle prompt instruction]
```

**Logic:**
1. If write-spec already completed with git reconciled → skip
2. Start agent execution with timeout (default 30min)
3. Wait for result
4. If not "completed" status → checkpoint and retry (RetryableTransient)
5. Merge agent lifecycle into checkpoint
6. Git reconcile: inspect head, commit uncommitted changes if any

**Errors:**
- Agent timeout/failure → RetryableTransient or RetryableAfterResume
- Git inspect/commit → RetryableAfterResume

### 1.4 publish
**GitHub API calls:**
- `Push()` — push branch
- `ListOpenPullRequests()` — find existing PR for branch
- `ViewPullRequest()` — validate lifecycle PR
- `CreatePullRequest()` — open spec PR
- `AddPullRequestLabels()` — add `"looper:spec-reviewing"` label
- `AddPullRequestReviewers()` — request reviewers
- `UpdatePullRequestBody()` — normalize disclosure stamp

**Logic:**
1. If `allowAutoPush` disabled → `ManualIntervention` skip
2. Push branch if not pushed yet
3. Try to adopt agent-created PR (via lifecycle state)
4. Try to adopt existing open PR for branch
5. Create PR with title `"Spec: {issue.title}"` and body with spec path
6. Add `looper:spec-reviewing` label
7. Add requested reviewers
8. Normalize PR body disclosure stamp

**Errors:**
- Push failure → RetryableAfterResume
- PR creation failure → RetryableAfterResume
- Label/add reviewer failure → RetryableAfterResume

### 1.5 notify
**Logic:**
1. Build notification message with spec PR URL
2. Set `checkpoint.Notify.SentAt`

---

## 2. Reviewer State Machine

### Step Sequence
```
discover → filter → claim → snapshot → review → publish
```

### Step Enums
```
stepDiscover = "discover"
stepFilter   = "filter"
stepClaim    = "claim"
stepSnapshot = "snapshot"
stepReview   = "review"
stepPublish  = "publish"
```

### 2.1 discover
**GitHub API calls:**
- `ViewPullRequest(repo, prNumber)` → full PR detail with reviews, comments, checks

**Logic:**
1. Fetch PR detail from GitHub
2. Store in checkpoint

### 2.2 filter
**Logic:**
1. Apply scope filters (drafts, labels, review decision, author)
2. Check if already reviewed by current user
3. Check review markers (find `<!-- looper:review -->` comment)
4. Evaluate approval criteria via `criteria` package

### 2.3 claim
**GitHub API calls:**
- Acquires lock `pr:{repo}:{number}`

### 2.4 snapshot
**GitHub API calls:**
- `CapturePullRequestSnapshot()` — capture PR state at this point in time

> **Sub-operation — worktree preparation:** as part of preparing for review,
> the PR branch is checked out to a worktree via `CreateWorktree()` and synced
> with remote (fetch + reset). This is NOT a separate step — it happens within
> the snapshot/review preparation phase.

**Sub-operation — thread resolution (optional, controlled by `ThreadResolution` config):**
- `ListReviewThreads()` — list all unresolved review threads
- `AddReviewThreadReply()` — reply to threads
- `ResolveReviewThread()` — resolve threads
- If disabled → skip
- Agent decides per thread whether to resolve or reply

### 2.5 review
**Agent prompt (review prompt):**
```
Review the pull request {repo}#{prNumber}.
Repository: {repo}
Base branch: {base}
PR title: {title}
PR body: {body}
[PR diff / code content]
[custom instructions]
[review guidelines]
```

**GitHub API calls:**
- `FindReviewMarker()` — check for existing review marker comment
- `SubmitReview()` — submit APPROVE/COMMENT/REQUEST_CHANGES
- `CreateIssueComment()` — post review comment
- `AddPullRequestReaction()` — react to comments
- `RemovePullRequestReaction()` — remove reactions

**Logic:**
1. Start agent execution for review
2. Agent returns structured output with review event (APPROVE/COMMENT/REQUEST_CHANGES)
3. Find any existing review marker to avoid duplicate reviews
4. Submit review via GitHub API
5. Handle agent-native review (AGENT_NATIVE event type)
6. Post issue comment with summary

### 2.8 publish
**Logic:**
1. Check auto-approve conditions
2. Enable auto-merge if criteria met (`EnableAutoMerge()`)
3. Add/remove labels (e.g., `looper:spec-ready`)
4. Update loop metadata

### Auto-Merge Logic
- Controlled by `AllowAutoApprove` config
- If review outcome is APPROVE:
  - Check merge criteria via `automerge` package
  - Call `EnableAutoMerge()` to enable GitHub auto-merge
  - Add ready labels

### Thread Resolution
- **Disabled**: skip step entirely
- **Before Review**: resolve threads before starting review
- **After Review**: resolve threads after submitting review
- Agent decides: reply to thread or resolve it
- For spec PRs: more aggressive resolution

---

## 3. Fixer State Machine

### Step Sequence
```
discover-pr → claim-pr → collect-fixes → prepare-worktree → repair → validate → push → reconcile-commits → resolve-comments → recheck
```

### Step Enums
```
stepDiscoverPR       = "discover-pr"
stepClaimPR          = "claim-pr"
stepCollectFixes     = "collect-fixes"
stepPrepareWorktree  = "prepare-worktree"
stepRepair           = "repair"
stepValidate         = "validate"
stepPush             = "push"
stepReconcileCommits = "reconcile-commits"
stepResolveComments  = "resolve-comments"
stepRecheck          = "recheck"
```

### 3.1 discover-pr
**GitHub API calls:**
- `ViewPullRequest(repo, prNumber)` → PR detail with conflicts, checks

### 3.2 claim-pr
**GitHub API calls:**
- Acquire lock `pr:{repo}:{number}`

### 3.3 collect-fixes
**Logic:**
1. If discovery was targeted (from webhook/comment) → load fix items from PR comments
2. Parse review comments to extract fix items (`FixItem` list)
3. Deduplicate fix items by thread fingerprint

### 3.4 prepare-worktree
**Git/GitHub API calls:**
- `CreateWorktree()` — checkout branch for fixing
- `PrepareWorktree()` — sync with remote

### 3.5 repair
**Agent prompt:**
```
Fix issues in PR {repo}#{prNumber}.
Repository: {repo}
Fix items: {list of FixItem}
[code diff / context]
[custom instructions]
```

**Logic:**
1. Start agent execution for repair
2. Agent produces code changes
3. Validate agent output (structured result)

### 3.6 validate
**Logic:**
1. Verify no compilation errors (via shell/infra)
2. Check for remaining issues

### 3.7 push
**Git API calls:**
- `Push()` — force push if needed
- `FetchBranch()`, `IsAncestor()` — safe-push validation

### 3.8 reconcile-commits
**Git API calls:**
- `InspectHead()` — check for changes
- `Commit()` — commit changes (with disclosure-stamped message)

### 3.9 resolve-comments
**GitHub API calls:**
- `ListReviewThreads()` — list threads
- `AddReviewThreadReply()` — reply to threads
- `ResolveReviewThread()` — resolve threads
- `CreateIssueComment()` — post summary

**Logic:**
1. List all review threads
2. For each unresolved thread:
   - If fix produced new commits → resolve
   - If no fix possible → reply explaining why
   - Fallback to `noopResolveManualIntervention` if no new commits

### 3.10 recheck
**Logic:**
1. Wait for check runs to complete (polling)
2. `CompareCommits()` — check if base branch advanced
3. If checks pass → done
4. If checks fail → loop back to repair

---

## 4. Worker State Machine

### Step Sequence
```
prepare-work → prepare-worktree → plan → execute → validate → open-pr
```

### Step Enums
```
stepPrepareWork     = "prepare-work"
stepPrepareWorktree = "prepare-worktree"
stepPlan            = "plan"
stepExecute         = "execute"
stepValidate        = "validate"
stepOpenPR          = "open-pr"
```

### 4.1 prepare-work
**GitHub API calls:**
- `ViewIssue(repo, issueNumber)` — get issue detail (for issue-based workers)
- `ViewPullRequest(repo, prNumber)` — get PR detail (for PR-based workers)
- `ListOpenPullRequests()` — check for existing branches
- `CompareBranches()` — check ahead/behind
- `GetCurrentUserLogin()` — resolve current user

**Logic:**
1. Determine target (issue or PR)
2. Check for existing branch/PR to resume
3. Build branch name: `looper/worker/{slug}-{hash}`

### 4.2 prepare-worktree
**Git API calls:**
- `CreateWorktree()` — checkout branch
- `RestoreWorktree()` — restore existing worktree for resume

### 4.3 plan
**Agent prompt (plan):**
```
Plan the implementation for {issue/PR}.
Repository: {repo}
Base branch: {base}
Target: {issue/PR detail}
[custom instructions]
```

**Logic:**
1. Start agent execution for planning
2. Agent produces structured plan
3. Store plan in checkpoint

### 4.4 execute
**Agent prompt (execute):**
```
Implement the planned changes for {issue/PR}.
Repository: {repo}
Plan: {plan from step 4.3}
Base branch: {base}
[custom instructions]
[lifecycle prompt]
```

**Logic:**
1. Start agent execution for implementation
2. Agent produces code changes, commits
3. Merge agent lifecycle into checkpoint
4. Git reconcile (similar to planner write-spec)

### 4.5 validate
**Logic:**
1. Inspect head for changes
2. Verify structured result output
3. Build PR body

### 4.6 open-pr
**GitHub API calls:**
- `CreatePullRequest()` — open PR
- `UpdatePullRequestBody()` — set PR body
- `UpdatePullRequestTitle()` — set PR title
- `AddPullRequestReviewers()` — add reviewers
- `AddPullRequestLabels()` — add labels
- `CreateIssueComment()` — comment on original issue
- `AddIssueAssignees()` — manage assignees

**Logic:**
1. Push branch if config allows
2. Create PR or adopt existing one for branch
3. Set labels and reviewers
4. Post status comment on original issue

---

## 5. Coordinator State Machine

The coordinator is not a step-based runner. It uses a **tick-based discovery** pipeline.

### Discovery Flow (`DiscoverIssues`)
```
shouldRunTick() → rate-limit check
ListOpenIssues() → up to 100 issues
LoadIssue() → for each: detail + timeline
applyMergeWatch() → re-trigger downstream actions for merged PRs
filterLoadedIssues() → exclude merge-watch issues
buildDependencyState():
  - Build dependency graph (blocked-by chains)
  - Manage sub-issue relationships
  - Track triage state
applyDependencyActions():
  - Post block/unblock comments
  - Manage dependency labels
  - Track cycle resolution
applyDispatches():
  - For each ready issue:
    - Check admission criteria
    - Route to local worker or network worker
    - Apply trigger labels (reviewer/fixer/worker)
applyReviewAssignments():
  - Add reviewers to PRs based on config
For each active issue (not merge-watch, not retriage):
  decide():
    - ShouldTriage / ShouldReTriage check
    - Inspect repository context
    - Call triage LLM → produce Decision
  applyDecision():
    - Add/remove labels (triaged, unclear, etc.)
    - Post triage comment
    - Schedule delayed label operations
```

### GitHub API calls used by coordinator:
- `ListOpenIssues()`, `ViewIssue()`, `ListIssueComments()`, `ListIssueTimeline()`
- `ListLinkedPullRequests()`, `ViewPullRequest()`
- `ListIssueBlockedBy()`, `GetIssueState()`
- `AddIssueLabels()`, `RemoveIssueLabels()`
- `CreateIssueComment()`, `UpdateIssueComment()`, `DeleteIssueComment()`
- `AddIssueAssignees()`, `AddIssueReaction()`
- `AddPullRequestReviewers()`, `AddPullRequestLabels()`, `RemovePullRequestLabels()`
- `ViewPullRequestMergeWatch()`, `ListPullRequestCheckRuns()`
- `GetBranchProtection()`, `GetRepositoryPermission()`
- `GetCurrentUserLogin()`, `GetCurrentUserLoginForRepo()`

### Triage Decision
```json
{
  "noOp": false,
  "markTriaged": true,
  "applyLabels": ["looper:triaged"],
  "removeLabels": ["looper:untriaged"],
  "clearLabelPatterns": ["triage/*"],
  "commentBody": "...",
  "reactionCommentId": 123,
  "reactionContent": "+1"
}
```

---

## 6. Shared Patterns Across All Runners

### 6.1 Checkpoint/Resume Mechanism
Every runner persists a JSON checkpoint after each step:
- `checkpoint.ResumePolicy` controls resume behavior:
  - `"replay_step"` — start from the beginning
  - `"retry_from_timeout_context"` — retry current step with agent timeout context
  - `"advance_from_checkpoint"` — skip completed steps
  - `"restart_from_discover"` — restart from discovery
  - `"manual_intervention"` — requires human action
  - `"rerun_review"` — specific to reviewer

### 6.2 Failure Kinds (shared enum)
```
FailureRetryableTransient   = "retryable_transient"
FailureRetryableAfterResume = "retryable_after_resume" 
FailureNonRetryable         = "non_retryable"
FailureManualIntervention   = "manual_intervention"
```

### 6.3 Step Execution Flow
```
for each step from startStep:
  persistStepStarted()
  executeStep() 
  if error:
    classify failure → failQueueItem() → completeRun("failed")
    updateLoop() → return
  if skipReason set:
    break
  persistStepCompleted()
completeRun("success" or "skipped")
completeQueueItem()
updateLoop("completed")
```

### 6.4 Deduplication Pattern
All runners use the same enqueue pattern:
```
FindActiveByDedupe() → if exists, return existing
CreateOrGetActiveByDedupe() → if created, wake scheduler
```

### 6.5 Spec PR Management
- **Labels:** `looper:spec-reviewing`, `looper:spec-ready`, `looper:needs-human`
- **Phases:** `PhaseSpec` (has `looper:spec-reviewing` label) vs `PhaseImplementation`
- **Spec path:** `specs/{YYYY-MM-DD}-{issueNumber}-{slug}.md`
- **PR body:** Contains `Spec: {path}` and `Issue: {repo}#{number}` in body
- **Promotion:** When reviewer finds spec clean → remove `looper:spec-reviewing`, add `looper:spec-ready`

---

## 7. Dispatch State Machine

### 7.1 Purpose

Determines if/when a triaged issue should be dispatched to the Planner or Worker. Supports two modes: **human-gated** (waits for a `/plan` or `/implement` slash command) and **autonomous** (auto-dispatches after a configurable delay).

### 7.2 Config Types

```rust
struct DispatchConfig {
    mode:       DispatchMode,    // "human-gated" | "autonomous"
    triaged_label: String,       // "looper:triaged"
    hold_label:  Option<String>, // "looper:hold" — prevents autonomous dispatch
    autonomous_delay: Duration,  // delay after triage before auto-dispatch
    allowed_users: Vec<String>,  // who can issue slash commands (empty = all with write access)
    slash_commands: Vec<String>, // ["/plan", "/implement"]
    assign_to: Option<String>,   // user to auto-assign
    planner_trigger_labels: Vec<String>,  // labels to add for "/plan"
    worker_trigger_labels: Vec<String>,   // labels to add for "/implement"
}

enum DispatchMode { HumanGated, Autonomous }

const DISPATCH_PLAN: &str = "dispatch/plan";
const DISPATCH_IMPLEMENT: &str = "dispatch/implement";
```

### 7.3 Core Decision Function

```
fn Decide(issue, cfg, now, dependency_graph) -> Action

Human-gated mode:
  1. Scan comments (newest-first) for slash command
  2. If no slash command found → NoOp
  3. Parse command from comment body: /plan or /implement
  4. Validate comment author: must be in allowed_users OR have write access
  5. If issue doesn't have triaged_label → post failure reaction (confused emoji)
  6. Check dispatch label: triage must have set exactly one of "dispatch/plan" or "dispatch/implement"
  7. Slash command must match the dispatch label (/plan → dispatch/plan)
  8. If trigger labels already applied → NoOp + success reaction (+1 emoji)
  9. If dependency graph has unsatisfied blockers → post failure comment
  10. Otherwise → return TriggerLabels to apply, AssignTo, success reaction

Autonomous mode:
  1. If no triaged label → NoOp
  2. If no single dispatch label → NoOp
  3. If hold label present → NoOp
  4. If trigger labels already applied → NoOp
  5. If !triaged_at + autonomous_delay → NoOp (not yet eligible)
  6. If unsatisfied dependency blockers → NoOp (wait)
  7. Otherwise → return TriggerLabels + AssignTo
```

### 7.4 Slash Command Parsing

```rust
fn parse_slash_command(body: &str, configured: &[String]) -> Option<&str> {
    // Iterate lines, skip code fences (```, ~~~) and blockquotes (>)
    // Match configured commands: "/plan", "/implement"
    // Command must be at word boundary: "/plan" at start of line, followed by space/tab/end
    // Return the matched command or None
}
```

Rules:
- Only commands in configured list are allowed (typically `["/plan", "/implement"]`)
- Commands inside code fences or blockquotes are ignored
- Must be at the start of the line (with optional leading whitespace)
- Command boundary = space, tab, or end-of-line

### 7.5 Label Logic

```rust
fn single_dispatch_label(labels: &[String]) -> Option<String> {
    // Find labels starting with "dispatch/"
    // If zero or more than one found → None (ambiguous)
    // If exactly one → Some(label)
}

fn trigger_labels_for_dispatch(dispatch_label: &str, cfg: &DispatchConfig) -> Vec<String> {
    match dispatch_label {
        "dispatch/plan"      => cfg.planner_trigger_labels,  // e.g. ["looper:plan"]
        "dispatch/implement" => cfg.worker_trigger_labels,   // e.g. ["looper:implement"]
        _                    => vec![]
    }
}

fn missing_labels(existing: &[String], want: &[String]) -> Vec<String> {
    // Return labels from want that aren't in existing
}
```

### 7.6 Dependency Gate

```rust
fn needs_dependency_gate(issue, cfg, now) -> bool {
    // For human-gated: slash command exists, triaged, dispatch label matches command,
    //   trigger labels configured, but some trigger labels are still missing
    // For autonomous: triaged, has dispatch label, no hold label, past delay,
    //   trigger labels configured but missing, no unsatisfied blockers
}
```

### 7.7 Dispatch Actions

```rust
struct DispatchAction {
    no_op:                bool,
    trigger_labels:       Vec<String>,
    assign_to:            Option<String>,
    reaction_comment_id:  i64,         // ID of the slash command comment to react on
    reaction_content:     String,      // "+1" for success, "confused" for failure
    failure_comment_body: Option<String>,  // posted as comment on failure
}
```

### 7.8 Edge Cases

| Scenario | Outcome |
|----------|---------|
| Multiple dispatch/ labels | NoOp — ambiguous triage, human must fix |
| Slash command from non-allowed user | Ignored, continue scanning older comments |
| Dependency gate active | Failure comment listing unsatisfied blockers |
| Auto-merge watch issue | Excluded from dispatch entirely |
| Hold label set (autonomous) | NoOp until hold removed |
| Autonomous delay not elapsed | NoOp, will retry on next tick |
| Trigger labels already present | NoOp + success reaction (already dispatched) |

---

## 8. MergeWatch Classifier

### 8.1 Purpose

Watches PRs that have auto-merge enabled and classifies their state to determine the next action. Used by the Coordinator tick loop to react to merge results, conflicts, CI failures, or transient errors.

### 8.2 Snapshot Model

```rust
struct PRSnapshot {
    repo:                    String,
    pr_number:               i64,
    issue_number:            i64,
    head_sha:                String,
    merged:                  bool,
    open:                    bool,
    auto_merge_enabled:      bool,
    auto_merge_owned_by_looper: bool,
    has_looper_label:        bool,
    mergeable:               Option<bool>,
    mergeable_state:         String,        // "clean" | "dirty" | "unknown" | "blocked" | "behind"
    required_checks:         RequiredCheckSummary,
    temporary_error:         Option<TemporaryError>,
}

struct RequiredCheckSummary {
    failed:  Vec<String>,     // CI checks that failed
    pending: Vec<String>,     // CI checks still running
    missing: Vec<String>,     // Expected checks not yet reported
}

struct TemporaryError {
    suggested_delay: Duration,  // e.g. 60s, back off and retry
}
```

### 8.3 Watch Actions

```rust
enum WatchActionKind {
    Merged,                    // PR was merged → trigger downstream (e.g., worktree cleanup)
    StillPending,              // Mergeable, CI passing, but not yet merged (wait)
    Indeterminate,             // Mergeable state unknown — still being computed by GitHub
    Conflict,                  // MergeableState == "dirty" — has merge conflicts
    RedCI,                     // One or more required checks failed
    BranchProtectionChanged,   // MergeableState unknown for too long, or missing required checks
    HumanDisabledAutoMerge,    // Auto-merge was manually disabled
    TransientError,            // Temporary GitHub API error (retry)
}

struct WatchAction {
    kind:              WatchActionKind,
    first_unknown_at:  Option<DateTime<Utc>>,
    deadline_exceeded: bool,
    retries_left:      usize,
    suggested_delay:   Duration,
    exhausted:         bool,       // true if retries have run out
}
```

### 8.4 Prior Watch Marker (State Persistence)

```rust
struct PriorWatchMarker {
    pr_number:        i64,
    head_sha:         String,          // reset tracking when SHA changes
    retries:          usize,
    first_unknown_at: Option<DateTime<Utc>>,
    next_retry_at:    Option<DateTime<Utc>>,
}
```

### 8.5 Retry Budget

```rust
struct RetryBudget {
    now:                       DateTime<Utc>,
    transient_retries:         usize,    // max transient retries before giving up
    max_indeterminate_duration: Duration, // how long "unknown" is tolerated
}
```

### 8.6 Classification Algorithm

```
fn Classify(snapshot: PRSnapshot, prior: Option<PriorWatchMarker>, budget: RetryBudget) -> WatchAction

1. Normalize prior:
   - If no prior OR pr_number/head_sha changed → reset with budget.transient_retries
   - Else → keep existing retry count

2. Evaluate in order:

   a. TemporaryError:
      - Decrement retries_left (unless new PR/SHA)
      - Return TransientError with retries_left + suggested_delay
      - If exhausted (retries_left == 0) → signal exhaustion

   b. Merged:
      - Return Merged

   c. AutoMerge disabled (and previously was enabled):
      - Return HumanDisabledAutoMerge

   d. Mergeable is nil OR mergeable_state == "unknown":
      - Track first_unknown_at
      - If max_indeterminate_duration exceeded → BranchProtectionChanged
      - Else → Indeterminate

   e. MergeableState == "dirty":
      - Return Conflict

   f. RequiredChecks.Failed is non-empty:
      - Return RedCI

   g. RequiredChecks.Missing is non-empty:
      - Return BranchProtectionChanged

   h. Otherwise:
      - Return StillPending
```

### 8.7 Edge Cases

| Scenario | Classification | Action |
|----------|---------------|--------|
| First tick, mergeable unknown, no prior | `ActionIndeterminate` | Track `first_unknown_at`, retry next tick |
| Unknown for > 5 minutes (configurable) | `ActionBranchProtectionChanged` | Skip, free capacity |
| GitHub returns 502/503 | `ActionTransientError` | Retry up to N times with backoff |
| Head SHA changed mid-watch | Reset prior | Fresh retry count |
| Auto-merge disabled by human | `ActionHumanDisabledAutoMerge` | Stop watching |
| PR merged successfully | `ActionMerged` | Trigger post-merge actions |
| Required check failed | `ActionRedCI` | Report, stop watching |
| Merge conflicts | `ActionConflict` | Report, stop watching |
