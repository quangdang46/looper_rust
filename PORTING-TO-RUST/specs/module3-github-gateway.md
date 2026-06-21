# Module 3: looper-github (Gateway) — Rust Port Spec

## Source Files
- `internal/infra/github/gateway.go` — 3694 lines
- `internal/infra/github/errors.go` — 177 lines
- `internal/infra/github/gateway_test.go` — references but NOT read for spec

---

## 1. GATEWAY STRUCT

```rust
const JAVA_SCRIPT_ISO_STRING_LAYOUT: &str = "2006-01-02T15:04:05.000Z";

// Timeout constants
const DEFAULT_GH_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const PR_LIST_GH_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const PR_DIFF_GH_COMMAND_TIMEOUT: Duration = Duration::from_secs(180);

// PR JSON field lists used for gh CLI --json arguments
const PR_LIST_JSON_FIELDS: &[&str] = &[
    "number", "title", "url", "state", "updatedAt", "isDraft", "reviewDecision",
    "labels", "headRefName", "baseRefName", "headRefOid", "baseRefOid",
    "author", "reviewRequests", "reviews", "mergeStateStatus"
];

const PR_DISCOVERY_LIST_JSON_FIELDS: &[&str] = &[
    "number", "title", "url", "state", "updatedAt", "isDraft", "reviewDecision",
    "labels", "headRefName", "baseRefName", "headRefOid", "baseRefOid",
    "author", "reviewRequests", "mergeStateStatus"
];

const PR_VIEW_JSON_FIELDS: &[&str] = &[
    "number", "title", "body", "url", "state", "createdAt", "updatedAt",
    "closedAt", "isDraft", "reviewDecision", "labels", "headRefName", "baseRefName",
    "headRefOid", "baseRefOid", "author", "reviewRequests", "comments",
    "reviews", "statusCheckRollup", "mergeStateStatus"
];

// Sentinels
const ERR_DIFF_TOO_LARGE: Error = ...;
```

### Options (constructor input)
```rust
struct GatewayOptions {
    gh_path: String,                                   // default: "gh"
    cwd: String,                                       // default: process CWD
    now: fn() -> Instant,                              // default: Instant::now
    discovery_cache_ttl: Duration,                     // default: from config
    gh_run: fn(ctx, ShellOptions) -> Result<ShellResult, Error>,  // default: shell::run
    review_submit_diagnostic: Option<fn(String, HashMap<String, Value>)>,
}
```

### Gateway
```rust
struct Gateway {
    gh_path: String,
    cwd: String,
    now: fn() -> Instant,
    discovery_cache_ttl: Duration,
    discovery_cache_mu: Mutex<()>,
    discovery_pr_cache: HashMap<String, DiscoveryPullRequestListCacheEntry>,
    discovery_review_pr_cache: HashMap<String, DiscoveryPullRequestListCacheEntry>,
    discovery_issue_cache: HashMap<String, DiscoveryIssueListCacheEntry>,
    gh_run: fn(ctx, ShellOptions) -> Result<ShellResult, Error>,
    review_submit_diagnostic: Option<fn(String, HashMap<String, Value>)>,
}

struct DiscoveryPullRequestListCacheEntry {
    expires_at: Instant,
    items: Vec<PullRequestSummary>,
}

struct DiscoveryIssueListCacheEntry {
    expires_at: Instant,
    items: Vec<IssueSummary>,
}
```

### Constructor
```rust
fn new(options: GatewayOptions) -> Gateway
```

---

## 2. TYPES (all input/output structs)

### PullRequestSummary
```rust
struct PullRequestSummary {
    number: i64,
    title: String,
    url: String,
    state: String,                 // "OPEN", "CLOSED", "MERGED"
    updated_at: String,            // ISO 8601
    is_draft: bool,
    review_decision: String,       // "APPROVED", "CHANGES_REQUESTED", "REVIEW_REQUIRED", ""
    labels: Vec<String>,
    head_ref_name: String,
    base_ref_name: String,
    head_sha: String,              // "headRefOid"
    base_sha: String,              // "baseRefOid"
    has_conflicts: bool,           // mergeStateStatus == "DIRTY"
    author: String,
    author_association: String,
    review_requests: Vec<String>,
    review_request_users: Vec<GitHubUser>,
    reviews: Vec<HashMap<String, Value>>,    // raw gh JSON
}
```

### PullRequestDetail
```rust
struct PullRequestDetail {
    number: i64,
    title: String,
    body: String,
    url: String,
    state: String,
    created_at: String,
    updated_at: String,
    closed_at: String,
    is_draft: bool,
    review_decision: String,
    labels: Vec<String>,
    head_ref_name: String,
    base_ref_name: String,
    head_sha: String,
    base_sha: String,
    author: String,
    author_association: String,
    comment_count: i32,
    review_requests: Vec<String>,
    review_request_users: Vec<GitHubUser>,
    has_conflicts: bool,
    comments: Vec<HashMap<String, Value>>,     // review threads (GraphQL shape)
    issue_comments: Vec<CommentInfo>,
    reviews: Vec<HashMap<String, Value>>,       // raw gh JSON
    checks: Vec<HashMap<String, Value>>,        // statusCheckRollup
    mergeable: Option<bool>,
    mergeable_state: String,
    merged_at: String,
    auto_merge: Option<PullRequestAutoMerge>,
}
```

### PullRequestAutoMerge
```rust
struct PullRequestAutoMerge {
    enabled_by: String,
    merge_method: String,          // "SQUASH", "MERGE", "REBASE"
}
```

### PullRequestCheckRuns
```rust
struct PullRequestCheckRuns {
    total_count: i32,
    check_runs: Vec<PullRequestCheckRun>,
    statuses: Vec<PullRequestStatus>,
}

struct PullRequestCheckRun {
    name: String,
    status: String,              // "QUEUED", "IN_PROGRESS", "COMPLETED"
    conclusion: String,          // "SUCCESS", "FAILURE", "NEUTRAL", "CANCELLED", "SKIPPED", "TIMED_OUT", "ACTION_REQUIRED"
}

struct PullRequestStatus {
    context: String,
    state: String,               // "pending", "success", "failure", "error"
}
```

### CommentInfo
```rust
struct CommentInfo {
    id: i64,
    author: String,
    author_association: String,
    body: String,
    created_at: String,
    updated_at: String,
    url: String,
}
```

### CurrentUserIdentity
```rust
struct CurrentUserIdentity {
    login: String,
    numeric_id: i64,
}
```

### Issues

```rust
struct IssueSummary {
    number: i64,
    title: String,
    body: String,
    url: String,
    state: String,
    updated_at: String,
    author: String,
    author_association: String,
    assignees: Vec<String>,
    assignee_users: Vec<GitHubUser>,
    labels: Vec<String>,
    is_pull_request: bool,
}

struct IssueDetail {
    number: i64,
    title: String,
    body: String,
    url: String,
    state: String,
    state_reason: String,
    created_at: String,
    updated_at: String,
    closed_at: String,
    author: String,
    author_association: String,
    assignees: Vec<String>,
    assignee_users: Vec<GitHubUser>,
    labels: Vec<String>,
    is_pull_request: bool,
    comment_count: i32,
    comments: Vec<CommentInfo>,
}

struct IssueRepository {
    name: String,
    full_name: String,
    url: String,
    html_url: String,
}

struct DependencyIssue {
    id: i64,
    number: i64,
    title: String,
    url: String,
    html_url: String,
    repository_url: String,
    state: String,
    state_reason: String,
    repository: IssueRepository,
}

struct IssueState {
    state: String,
    state_reason: String,
}
```

### Input types (used as function parameters)

```rust
struct IssueTimelineInput { repo: String, issue_number: i64, cwd: String }
struct IssueReactionInput { repo: String, issue_number: i64, comment_id: i64, cwd: String }
struct CreateIssueReactionInput { repo: String, issue_number: i64, comment_id: i64, content: String, cwd: String }
struct RepositoryPermissionInput { repo: String, user: String, cwd: String }
struct ListIssueBlockedByInput { repo: String, issue_number: i64, cwd: String }
struct IssueDependency { number: i64, repo: String }
struct LinkedPullRequestsInput { repo: String, issue_number: i64, cwd: String }
struct PullRequestReviewStateInput { repo: String, pr_number: i64, cwd: String }
struct IssueReaction { id: i64, content: String, user_login: String }
struct LinkedPullRequest { number: i64, state: String, merged: bool, merged_at: String, merge_commit_sha: String }
struct PullRequestReviewState { requested_reviewers: Vec<String>, latest_review_per_user: HashMap<String, String>, last_review_at: String }
struct GitHubUser { login: String, id: i64 }
struct PullRequestHeadAndAuthor { head_sha: String, author: String }

struct RepositorySettingsInput { repo: String, cwd: String }
struct RepositorySettings { allow_squash_merge: bool, allow_merge_commit: bool, allow_rebase_merge: bool, allow_auto_merge: bool }
struct BranchProtectionInput { repo: String, branch: String, cwd: String }
struct BranchProtection { enabled: bool, has_required_checks: bool, required_checks: Vec<String> }

struct IssueCommentInput { repo: String, issue_number: i64, body: String, cwd: String }
struct IssueAssigneesInput { repo: String, issue_number: i64, assignees: Vec<String>, cwd: String }
struct IssueLabelsInput { repo: String, issue_number: i64, labels: Vec<String>, cwd: String }
struct IssueCommentResult { id: i64, url: String }
struct UpdateIssueCommentInput { repo: String, comment_id: i64, body: String, cwd: String }
struct DeleteIssueCommentInput { repo: String, comment_id: i64, cwd: String }
struct CloseIssueInput { repo: String, issue_number: i64, state_reason: String, cwd: String }
struct ClosePullRequestInput { repo: String, pr_number: i64, cwd: String }

struct EnableAutoMergeInput { repo: String, pr_number: i64, strategy: ReviewerAutoMergeStrategy, head_sha: String, cwd: String }
struct PullRequestCheckRunsInput { repo: String, r#ref: String, cwd: String }

struct SubmitReviewInput {
    repo: String,
    pr_number: i64,
    event: String,               // "APPROVE", "REQUEST_CHANGES", "COMMENT"
    body: String,
    commit_id: String,
    comments: Vec<ReviewComment>,
    anchors: Option<diffanchor::Index>,   // for validation/fixup
    disclosure: DisclosureConfig,
    cwd: String,
}

struct ReviewComment {
    body: String,
    path: String,
    line: i64,
    side: String,                // "LEFT", "RIGHT"
    start_line: i64,
    start_side: String,
    diagnostic_index: i32,
}

struct VerifyReviewMarkerInput { repo: String, pr_number: i64, marker: String, allowed_review_events: Vec<String>, author_login: String, allow_clean_comment: bool, cwd: String }

struct ReviewMarkerResult { found: bool, outcome: String, event: String, author_login: String, body: String, review_id: String, inline_comment_bodies: Vec<String> }

struct PullRequestReactionInput { repo: String, pr_number: i64, content: String, cwd: String }
struct PullRequestCommentInput { repo: String, pr_number: i64, body: String, cwd: String }
struct PullRequestLabelsInput { repo: String, pr_number: i64, labels: Vec<String>, cwd: String }
struct PullRequestReviewersInput { repo: String, pr_number: i64, reviewers: Vec<String>, cwd: String }

struct CreatePullRequestInput { repo: String, head_branch: String, base_branch: String, title: String, body: String, cwd: String }
struct CreatePullRequestResult { number: i64, url: String }

struct CompareBranchesInput { repo: String, base_branch: String, head_branch: String, cwd: String }
struct CompareBranchesResult { ahead_by: i32, behind_by: i32, status: String, total_commits: i32 }   // JSON: "ahead_by","behind_by","status","total_commits"

struct UpdatePullRequestTitleInput { repo: String, pr_number: i64, title: String, cwd: String }
struct UpdatePullRequestBodyInput { repo: String, pr_number: i64, body: String, cwd: String }

struct ListOpenPullRequestsInput { repo: String, cwd: String, limit: i32, label: String, labels: Vec<String>, author: String, base_ref_name: String, timeout: Duration }
struct ListReviewRequestedPullRequestsInput { repo: String, cwd: String, limit: i32, reviewer: String, timeout: Duration }
struct ListOpenIssuesInput { repo: String, cwd: String, limit: i32, assignee: String, label: String, labels: Vec<String> }
struct ViewIssueInput { repo: String, issue_number: i64, cwd: String }
struct ViewPullRequestInput { repo: String, pr_number: i64, cwd: String }
struct ResolveReviewThreadInput { repo: String, thread_id: String, cwd: String }
struct ListReviewThreadsInput { repo: String, pr_number: i64, cwd: String, limit: i32 }
struct ViewReviewThreadInput { thread_id: String, cwd: String }

struct ReviewThread {
    id: String,
    is_resolved: bool,
    path: String,
    line: i64,
    url: String,
    comments: Vec<ReviewThreadComment>,
}

struct ReviewThreadComment {
    id: String,
    body: String,
    author: String,
    author_association: String,
    created_at: String,
    updated_at: String,
    path: String,
    line: i64,
    original_commit_oid: String,
    commit_oid: String,
    url: String,
}

struct AddReviewThreadReplyInput { repo: String, thread_id: String, body: String, cwd: String }
struct CompareCommitsInput { repo: String, base: String, head: String, cwd: String }
struct CompareCommitsResult { status: String }
struct GetPullRequestDiffInput { repo: String, pr_number: i64, cwd: String }

struct CapturePullRequestSnapshotInput { project_id: String, repo: String, pr_number: i64, cwd: String, captured_at: String }

struct InitializeLabelsInput { repo: String, cwd: String, dry_run: bool }

struct LabelDefinition {
    name: String,        // json:"name"
    color: String,       // json:"color"
    description: String, // json:"description"
}

struct LabelInitResult {
    repo: String,          // json:"repo"
    dry_run: bool,          // json:"dryRun"
    labels: Vec<LabelInitItem>,  // json:"labels"
    summary: LabelInitSummary,   // json:"summary"
}

struct LabelInitItem {
    name: String,        // json:"name"
    status: String,      // json:"status" — "created", "updated", "skipped", "failed"
    color: String,       // json:"color"
    description: String, // json:"description"
    error: String,       // json:"error,omitempty"
}

struct LabelInitSummary {
    created: i32,  // json:"created"
    updated: i32,  // json:"updated"
    skipped: i32,  // json:"skipped"
    failed: i32,   // json:"failed"
}

struct ReviewThreadNotFoundError { thread_id: String }
```

### Internal types (not exported from Go but used)
```rust
struct reviewIdempotencyMarker { id: String, head: String, outcome: String }  // private
struct githubReaction { id: i64, content: String, user_login: String }       // private
struct reviewThreadNode { id: String, is_resolved: bool }                     // private
struct reviewSubmitHTTPResponse { status_code: i32, headers: HashMap<String, Value>, body: String }  // private

// Phoenix: review.comment processing summary
struct reviewCommentProcessing {
    original_count: usize,
    submitted_count: usize,
    normalized_count: usize,
    downgraded_count: usize,
    dropped_count: usize,
    comments: Vec<HashMap<String, Value>>,
}
```

---

## 3. PUBLIC METHODS (with signatures)

```rust
impl Gateway {
    // === DISCOVERY / LISTING ===

    fn list_open_pull_requests(
        &self, ctx, input: ListOpenPullRequestsInput
    ) -> Result<Vec<PullRequestSummary>>
    // Supports discovery snapshot context; uses gh pr list --json <fields>

    fn list_review_requested_pull_requests(
        &self, ctx, input: ListReviewRequestedPullRequestsInput
    ) -> Result<Vec<PullRequestSummary>>
    // Uses GraphQL search: repo:<repo> is:pr is:open review-requested:<reviewer>

    fn list_open_issues(
        &self, ctx, input: ListOpenIssuesInput
    ) -> Result<Vec<IssueSummary>>
    // Uses gh issue list --json number,title,body,url,state,updatedAt,author,assignees,labels

    fn view_issue(&self, ctx, input: ViewIssueInput) -> Result<IssueDetail>
    // Uses gh api repos/<repo>/issues/<number> + repos/<repo>/issues/<number>/comments (paginated, slurped)

    fn get_issue_state(&self, ctx, input: ViewIssueInput) -> Result<IssueState>
    // Uses gh api repos/<repo>/issues/<number>

    fn list_issue_blocked_by(&self, ctx, input: ListIssueBlockedByInput) -> Result<Vec<IssueDependency>>
    // Uses gh api repos/<repo>/issues/<number>/dependencies/blocked_by (paginated)

    fn list_blocked_by_issues(&self, ctx, input: ViewIssueInput) -> Result<Vec<DependencyIssue>>
    fn list_blocking_issues(&self, ctx, input: ViewIssueInput) -> Result<Vec<DependencyIssue>>
    fn list_sub_issues(&self, ctx, input: ViewIssueInput) -> Result<Vec<DependencyIssue>>
    // Uses gh api repos/<repo>/issues/<number>/dependencies/blocked_by|blocking|sub_issues (paginated, slurped)

    fn find_any_issue_number(&self, ctx, repo: &str, cwd: &str) -> Result<i64>
    // Paginates through all issues, skips PRs, returns first issue number found

    fn list_issue_comments(&self, ctx, input: ViewIssueInput) -> Result<Vec<CommentInfo>>
    // gh api repos/.../issues/<n>/comments --paginate --slurp

    fn list_issue_timeline(&self, ctx, input: IssueTimelineInput) -> Result<Vec<HashMap<String, Value>>>
    // gh api repos/.../issues/<n>/timeline --paginate --slurp

    fn list_issue_reactions(&self, ctx, input: IssueReactionInput) -> Result<Vec<IssueReaction>>
    // gh api repos/.../issues/<n>/reactions or issues/comments/<id>/reactions

    fn add_issue_reaction(&self, ctx, input: CreateIssueReactionInput) -> Result<()>
    // gh api repos/.../issues/<n>/reactions --method POST -H "Accept: application/vnd.github+json" -f content=<content>

    // === ISSUE COMMENTS ===

    fn create_issue_comment(&self, ctx, input: IssueCommentInput) -> Result<IssueCommentResult>
    // gh api repos/.../issues/<n>/comments --method POST -f body=<body>

    fn update_issue_comment(&self, ctx, input: UpdateIssueCommentInput) -> Result<()>
    // gh api repos/.../issues/comments/<id> --method PATCH -f body=<body>

    fn delete_issue_comment(&self, ctx, input: DeleteIssueCommentInput) -> Result<()>
    // gh api repos/.../issues/comments/<id> --method DELETE

    fn close_issue(&self, ctx, input: CloseIssueInput) -> Result<()>
    // gh issue close <number> --repo <repo> --reason <reason>
    // Idempotent: checks state first; retries if state reached

    fn add_issue_assignees(&self, ctx, input: IssueAssigneesInput) -> Result<()>
    // gh api repos/.../issues/<n>/assignees --method POST -f assignees[]=<user>

    fn add_issue_labels(&self, ctx, input: IssueLabelsInput) -> Result<()>
    // Creates labels if missing, then gh api repos/.../issues/<n>/labels --method POST -f labels[]=<label>

    fn remove_issue_labels(&self, ctx, input: IssueLabelsInput) -> Result<()>
    // gh api repos/.../issues/<n>/labels/<label> --method DELETE (per label)

    fn get_repository_permission(&self, ctx, input: RepositoryPermissionInput) -> Result<String>
    // gh api repos/<repo>/collaborators/<user>/permission → returns "admin","write","read","none"

    fn get_repository_settings(&self, ctx, input: RepositorySettingsInput) -> Result<RepositorySettings>
    // gh api repos/<repo> → allow_squash_merge, allow_merge_commit, allow_rebase_merge, allow_auto_merge

    fn get_branch_protection(&self, ctx, input: BranchProtectionInput) -> Result<BranchProtection>
    // gh api repos/<repo>/branches/<branch>/protection — extracts required_status_checks

    // === PULL REQUESTS ===

    fn view_pull_request(&self, ctx, input: ViewPullRequestInput) -> Result<PullRequestDetail>
    // gh pr view <n> --repo <r> --json <fields> + fetches review threads (GraphQL)

    fn view_pull_request_merge_watch(&self, ctx, input: ViewPullRequestInput) -> Result<PullRequestDetail>
    // gh api repos/<repo>/pulls/<n> — REST API for merge status specifically

    fn get_pull_request_author(&self, ctx, input: ViewPullRequestInput) -> Result<String>
    fn get_pull_request_head_and_author(&self, ctx, input: ViewPullRequestInput) -> Result<PullRequestHeadAndAuthor>

    fn list_pull_request_check_runs(&self, ctx, input: PullRequestCheckRunsInput) -> Result<PullRequestCheckRuns>
    // gh api repos/.../commits/<sha>/check-runs + .../commits/<sha>/status

    fn list_linked_pull_requests(&self, ctx, input: LinkedPullRequestsInput) -> Result<Vec<LinkedPullRequest>>
    // GraphQL: repository.issue.closedByPullRequestsReferences (paginated)

    fn list_pull_request_review_state(&self, ctx, input: PullRequestReviewStateInput) -> Result<PullRequestReviewState>
    // gh pr view <n> --repo <r> --json reviewRequests,reviews

    fn close_pull_request(&self, ctx, input: ClosePullRequestInput) -> Result<()>
    // gh pr close <n> --repo <r> (idempotent)

    fn enable_auto_merge(&self, ctx, input: EnableAutoMergeInput) -> Result<()>
    // gh pr merge <n> --repo <r> --auto --<strategy> --match-head-commit <sha>

    fn get_pull_request_head_sha(&self, ctx, input: ViewPullRequestInput) -> Result<String>
    // gh pr view <n> --repo <r> --json headRefOid

    // === REVIEW THREADS ===

    fn resolve_review_thread(&self, ctx, input: ResolveReviewThreadInput) -> Result<()>
    // GraphQL mutation: resolveReviewThread

    fn view_review_thread(&self, ctx, input: ViewReviewThreadInput) -> Result<ReviewThread>
    // GraphQL: node(id: <threadId>) → PullRequestReviewThread → comments (paginated)

    fn list_review_threads(&self, ctx, input: ListReviewThreadsInput) -> Result<Vec<ReviewThread>>
    // GraphQL: repository.pullRequest.reviewThreads (paginated)

    fn add_review_thread_reply(&self, ctx, input: AddReviewThreadReplyInput) -> Result<()>
    // GraphQL mutation: addPullRequestReviewThreadReply

    fn compare_commits(&self, ctx, input: CompareCommitsInput) -> Result<CompareCommitsResult>
    // gh api repos/<repo>/compare/<base>...<head> → returns status

    fn get_pull_request_diff(&self, ctx, input: GetPullRequestDiffInput) -> Result<String>
    // gh pr diff <n> --repo <r> (timeout: 180s)

    // === REVIEW SUBMISSION ===

    fn submit_review(&self, ctx, input: SubmitReviewInput) -> Result<()>
    // Complex: normalizes anchors, validates quality gates, handles disclosure
    // With inline comments: gh api repos/.../pulls/<n>/reviews --method POST --input -
    // Without: gh pr review <n> --repo <r> --<event> --body <body>
    // See detailed processing below

    fn add_pull_request_comment(&self, ctx, input: PullRequestCommentInput) -> Result<()>
    // gh pr comment <n> --repo <r> --body <body>

    fn has_review_marker(&self, ctx, input: VerifyReviewMarkerInput) -> Result<bool>
    fn find_review_marker(&self, ctx, input: VerifyReviewMarkerInput) -> Result<ReviewMarkerResult>
    // gh api repos/.../pulls/<n>/reviews --paginate --slurp → finds idempotency marker

    fn add_pull_request_reaction(&self, ctx, input: PullRequestReactionInput) -> Result<()>
    // gh api repos/.../issues/<n>/reactions --method POST -f content=<content>

    fn remove_pull_request_reaction(&self, ctx, input: PullRequestReactionInput) -> Result<()>
    // Lists reactions, finds matching one for current user, deletes it

    fn add_pull_request_labels(&self, ctx, input: PullRequestLabelsInput) -> Result<()>
    // Creates labels if missing, then gh api repos/.../issues/<n>/labels --method POST

    fn remove_pull_request_labels(&self, ctx, input: PullRequestLabelsInput) -> Result<()>
    // Per-label gh api repos/.../issues/<n>/labels/<label> --method DELETE

    fn add_pull_request_reviewers(&self, ctx, input: PullRequestReviewersInput) -> Result<()>
    // gh api repos/.../pulls/<n>/requested_reviewers --method POST -f reviewers[]=<user>

    fn create_pull_request(&self, ctx, input: CreatePullRequestInput) -> Result<CreatePullRequestResult>
    // gh pr create --repo <r> --head <h> --base <b> --title <t> --body <body>
    // Parses PR URL to extract number

    fn compare_branches(&self, ctx, input: CompareBranchesInput) -> Result<CompareBranchesResult>
    // gh api repos/<repo>/compare/<base>...<head> → JSON with "ahead_by","behind_by","status","total_commits"

    fn update_pull_request_title(&self, ctx, input: UpdatePullRequestTitleInput) -> Result<()>
    fn update_pull_request_body(&self, ctx, input: UpdatePullRequestBodyInput) -> Result<()>
    // gh pr edit <n> --repo <r> --title/--body

    // === AUTH / USER / REPO ===

    fn is_authenticated(&self, ctx, cwd: &str, hostname: &str) -> Result<bool>
    // gh auth status [--hostname <h>]

    fn get_current_user_login(&self, ctx, cwd: &str) -> Result<String>
    // gh api user --jq .login (falls back to GraphQL if token type blocks)

    fn get_current_user_identity(&self, ctx, cwd: &str) -> Result<CurrentUserIdentity>
    // gh api user --jq '{login: .login, id: .id}'

    fn get_current_user_login_for_repo(&self, ctx, repo: &str, cwd: &str) -> Result<String>
    fn detect_current_repository(&self, ctx, cwd: &str) -> Result<String>
    // gh repo view --json nameWithOwner,url

    fn initialize_labels(&self, ctx, input: InitializeLabelsInput) -> Result<LabelInitResult>
    // Creates/updates standard looper labels

    fn capture_pull_request_snapshot(&self, ctx, input: CapturePullRequestSnapshotInput) -> Result<PullRequestSnapshotRecord>
    // Views PR, gets diff, constructs storage record
}

// === STANDARD LABELS ===
fn standard_looper_labels() -> Vec<LabelDefinition>
// Returns: [looper:plan, looper:spec-reviewing, looper:spec-ready, looper:needs-human]
```

---

## 4. REVIEW SUBMIT FLOW

The `submit_review` method is the most complex function. It:

1. **Builds diagnostic request map** with repo, pr_number, event, commit_id, body summary, comments summary
2. **Checks idempotency** — if clean review marker found with comments, rejects
3. **Normalizes review anchors** — validates inline comments against diffanchor::Index, downgrades invalid ones to top-level
4. **Runs quality gate** — rejects if review quality flags found (e.g., comment without location context)
5. **Normalizes inline review disclosure** — strips/transforms disclosure stamps per config
6. **Submits via REST API** (if inline comments or commit_id):
   ```bash
   gh api repos/<repo>/pulls/<n>/reviews --method POST --input -
   ```
   with JSON payload:
   ```json
   {
     "event": "APPROVE|REQUEST_CHANGES|COMMENT",
     "body": "<body or null>",
     "commit_id": "<sha or null>",
     "comments": [
       {"body": "...", "path": "...", "line": 42, "side": "RIGHT", "start_line": 10, "start_side": "RIGHT"}
     ]
   }
   ```
7. **Falls back to gh CLI** (no inline comments):
   ```bash
   gh pr review <n> --repo <r> --approve|--request-changes|--comment --body <body>
   ```
8. **Emits diagnostics** at each major step (prepared, validation_failed, failed)

---

## 5. CACHE LOGIC

The gateway has three read-through caches for discovery operations:

```rust
// Cache entries
discovery_pr_cache: HashMap<String, DiscoveryPullRequestListCacheEntry>
discovery_review_pr_cache: HashMap<String, DiscoveryPullRequestListCacheEntry>
discovery_issue_cache: HashMap<String, DiscoveryIssueListCacheEntry>

struct DiscoveryPullRequestListCacheEntry {
    expires_at: Instant,
    items: Vec<PullRequestSummary>,
}

struct DiscoveryIssueListCacheEntry {
    expires_at: Instant,
    items: Vec<IssueSummary>,
}

// Cache key: "<repo>|<filters>" (e.g. "owner/repo||" for basic, or with label/author)
// TTL: from config.discovery_cache_ttl (default 30s)
// Lock: discovery_cache_mu (sync.Mutex) around read/write
```

There's also a **discovery snapshot** context mechanism where a snapshot can be injected via context to bypass cache and real API calls — used for testing and coordinated discovery.

---

## 6. ERROR TYPES (errors.go)

```rust
struct TransientError { inner: Box<dyn Error> }
// Error(): "transient GitHub error: {inner}"

fn is_transient_error(err: &Error) -> bool
// Checks for *TransientError or CommandExecutionError with transient messages

fn error_message(err: &Error) -> String
// Extracts most useful user-facing text, combining shell stderr/stdout

fn is_pull_request_not_found_error(err: &Error) -> bool
// Checks for "could not resolve to a pullrequest"

fn is_not_found_error(err: &Error) -> bool
// Checks for "http 404" or " 404" in combined output

fn is_inaccessible_review_request_reviewer_error(err: &Error) -> bool
// "resource not accessible" + "reviewrequests" + "requestedreviewer"
```

**Transient error detection** — messages that trigger automatic retry:
```
tls handshake timeout, unexpected eof, connection reset by peer,
connection refused, connection timed out, i/o timeout,
temporary failure in name resolution, no such host,
network is unreachable, stream error, http2: server sent goaway,
http 502, 502 bad gateway, http 503, 503 service unavailable,
http 504, 504 gateway timeout, secondary rate limit,
rate limit exceeded, api rate limit exceeded,
graphql: something went wrong
```

---

## 7. INTERNAL HELPER FUNCTIONS (used across gateway)

```rust
// === gh CLI execution ===
fn run_gh(&self, ctx, cwd: &str, stdin: &str, args: &[&str]) -> Result<ShellResult>
// Wraps run_gh_with_timeout with default timeout (60s)

fn run_gh_with_timeout(&self, ctx, cwd: &str, stdin: &str, timeout: Duration, args: &[&str]) -> Result<ShellResult>
// Calls self.gh_run, wraps transient errors in TransientError

// === JSON parsing ===
fn decode_json_object(value: &str) -> Result<HashMap<String, Value>>
fn decode_json_array(value: &str) -> Result<Vec<HashMap<String, Value>>>
fn decode_json_array_or_pages(value: &str) -> Result<Vec<HashMap<String, Value>>>

// === Field extraction (applied to gh JSON rows) ===
fn as_string(value: &Value) -> String
fn as_bool(value: &Value) -> bool
fn as_i64(value: &Value) -> i64
fn bool_ptr_from_value(value: &Value) -> Option<bool>
fn to_object_slice(value: &Value) -> Vec<HashMap<String, Value>>
fn nested_string(value: &HashMap<String, Value>, path: &[&str]) -> String
fn first_non_empty(values: &[&str]) -> String
fn first_non_nil(values: &[&Value]) -> Option<&Value>

// === Author/label extraction ===
fn extract_author(value: &Value) -> String              // map["login"] or map["name"]
fn extract_oid(value: &Value) -> String                 // map["oid"]
fn extract_label_names(value: &Value) -> Vec<String>    // array of {name: "..."}
fn extract_label_names_from_connection(value: &Value) -> Vec<String>
fn extract_review_request_logins(value: &Value) -> Vec<String>
fn extract_review_request_users(value: &Value) -> Vec<GitHubUser>
fn extract_actor_logins(value: &Value) -> Vec<String>
fn extract_actor_users(value: &Value) -> Vec<GitHubUser>
fn extract_comment_infos(value: &Value) -> Vec<CommentInfo>
fn extract_dependency_issue(value: &HashMap<String, Value>, default_repo: &str) -> DependencyIssue
fn extract_issue_repository(value: &Value) -> IssueRepository
fn extract_auto_merge(value: &Value) -> Option<PullRequestAutoMerge>

// === Repository helpers ===
fn parse_repo(repo: &str) -> Result<(String, String)>    // owner, name
fn split_repo_hostname(repo: &str) -> (String, String)   // hostname or "", repo
fn split_repo_owner_name(repo: &str) -> (String, String)  // owner, name
fn host_qualified_repo(name_with_owner: &str, repo_url: &str) -> String
fn validate_github_repo_slug(repo: &str) -> Result<()>

// === Review marker parsing ===
fn find_review_idempotency_marker(body: &str, marker: &str) -> Option<reviewIdempotencyMarker>
fn parse_review_idempotency_markers(body: &str) -> Vec<reviewIdempotencyMarker>
fn review_event_from_state(state: &str) -> &str    // "APPROVED"→"APPROVE", "COMMENTED"→"COMMENT", "CHANGES_REQUESTED"→"REQUEST_CHANGES"
fn review_event_allowed(event: &str, allowed: &[String]) -> bool

// === Utility ===
fn default_limit(limit: i32) -> i32          // min 30
fn random_id() -> String                     // UUID v4
fn encode_uri_component(value: &str) -> String
fn string_ptr(value: String) -> Option<String>
fn string_ptr_if_not_empty(value: &str) -> Option<String>
fn empty_to_nil(value: &str) -> Option<&str>
fn value_or(value: &str, fallback: &str) -> &str
fn unique_strings(values: &[String]) -> Vec<String>
fn summarize_checks(checks: &[HashMap<String, Value>]) -> String
fn count_unresolved_threads(comments: &[HashMap<String, Value>]) -> i32
fn parse_pr_number_from_url(url: &str) -> i64
fn normalize_github_login(login: &str) -> String
fn is_diff_too_large_error(err: &Error) -> bool

// === Labels ===
fn resolve_label_color(label: &str) -> &str
fn resolve_label_description(label: &str) -> &str
fn normalize_label_color(value: &str) -> String
fn increment_label_summary(summary: &mut LabelInitSummary, status: &str)

// === Review submit helpers ===
fn review_submit_request(input: &SubmitReviewInput) -> HashMap<String, Value>
fn review_submit_body_marker_summary(body: &str) -> HashMap<String, Value>
fn review_submit_comments_summary(comments: &[ReviewComment]) -> Vec<HashMap<String, Value>>
fn normalize_inline_review_disclosure(body: &str, disclosure_cfg: &DisclosureConfig) -> String
fn has_inline_review_disclosure(body: &str) -> bool
fn contains_visible_inline_review_disclosure(body: &str) -> bool
fn strip_inline_review_disclosure(body: &str) -> String

// === Review thread GraphQL helpers ===
fn fetch_review_threads(...) -> Result<Vec<HashMap<String, Value>>>
fn fetch_review_thread_page(...) -> Result<(Vec<Value>, String, bool, Error)>
fn fetch_review_threads_summary_page(...) -> Result<(Vec<Value>, String, bool, Error)>
fn fetch_review_thread_comments_page(...) -> Result<(Vec<Value>, String, bool, Error)>
fn fetch_linked_pull_requests_page(...) -> Result<(Vec<HashMap<String, Value>>, String, bool, Error)>
fn decode_review_threads_response(stdout: &str) -> Result<(Vec<Value>, String, bool, Error)>
fn normalize_review_thread(value: &Value) -> Option<HashMap<String, Value>>
fn review_thread_fingerprint_from_nodes(nodes: &[Value]) -> String
fn get_review_thread(...) -> Result<Option<reviewThreadNode>>
fn append_review_thread_comment(dst: &mut Vec<ReviewThreadComment>, nodes: &[Value])

// === Label management ===
fn ensure_labels_exist(...) -> Result<()>
fn list_repository_labels(...) -> Result<HashMap<String, LabelDefinition>>
```

---

## 8. GRAPHQL QUERIES USED

### Review Threads (main listing query)
```graphql
query($owner: String!, $name: String!, $prNumber: Int!, $limit: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $prNumber) {
      reviewThreads(first: $limit, after: $after) {
        nodes {
          id isResolved path line
          comments(first: 100) {
            nodes {
              id body createdAt updatedAt path line url authorAssociation
              author { login }
              originalCommit { oid }
              commit { oid }
            }
            pageInfo { hasNextPage endCursor }
          }
        }
        pageInfo { hasNextPage endCursor }
      }
    }
  }
}
```

### Review Thread (single by ID)
```graphql
query($threadId: ID!, $after: String) {
  node(id: $threadId) {
    ... on PullRequestReviewThread {
      comments(first: 100, after: $after) {
        nodes {
          id body createdAt updatedAt path line url authorAssociation
          author { login }
          originalCommit { oid }
          commit { oid }
        }
        pageInfo { hasNextPage endCursor }
      }
    }
  }
}
```

### Resolve Review Thread (mutation)
```graphql
mutation($threadId: ID!) {
  resolveReviewThread(input: { threadId: $threadId }) {
    thread { id isResolved }
  }
}
```

### Add Review Thread Reply (mutation)
```graphql
mutation($threadId: ID!, $body: String!) {
  addPullRequestReviewThreadReply(input: { pullRequestReviewThreadId: $threadId, body: $body }) {
    comment { id }
  }
}
```

### Linked Pull Requests
```graphql
query($owner: String!, $repo: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $repo) {
    issue(number: $number) {
      closedByPullRequestsReferences(first: 20, after: $after) {
        nodes { number state mergedAt mergeCommit { oid } }
        pageInfo { hasNextPage endCursor }
      }
    }
  }
}
```

### Search PRs by Review Requested
```graphql
query($searchQuery: String!, $first: Int!) {
  search(type: ISSUE, query: $searchQuery, first: $first) {
    nodes {
      ... on PullRequest {
        number title url state updatedAt isDraft reviewDecision
        labels(first: 20) { nodes { name } }
        headRefName baseRefName headRefOid baseRefOid mergeStateStatus
        author { login }
      }
    }
  }
}
```

### Get Viewer Login
```graphql
query { viewer { login } }
```

---

## 9. GH CLI COMMAND PATTERNS

| Operation | CLI Command |
|-----------|-------------|
| List PRs | `gh pr list --repo <r> --state open --limit <n> [--label <l>] [--author <a>] [--base <b>] --json <fields>` |
| View PR | `gh pr view <n> --repo <r> --json <fields>` |
| PR diff | `gh pr diff <n> --repo <r>` |
| PR comment | `gh pr comment <n> --repo <r> --body <body>` |
| PR review (simple) | `gh pr review <n> --repo <r> --approve\|--request-changes\|--comment --body <body>` |
| PR review (with comments) | `gh api repos/<r>/pulls/<n>/reviews --method POST --input -` |
| PR close | `gh pr close <n> --repo <r>` |
| PR merge (auto) | `gh pr merge <n> --repo <r> --auto --<strategy> --match-head-commit <sha>` |
| PR edit title | `gh pr edit <n> --repo <r> --title <t>` |
| PR edit body | `gh pr edit <n> --repo <r> --body <b>` |
| PR create | `gh pr create --repo <r> --head <h> --base <b> --title <t> --body <b>` |
| List issues | `gh issue list --repo <r> --state open --limit <n> [--assignee <a>] [--label <l>] --json <fields>` |
| Issue close | `gh issue close <n> --repo <r> --reason <reason>` |
| Label create | `gh label create <name> --repo <r> --color <c> --description <d> [--force]` |
| Label edit | `gh label edit <name> --repo <r> --color <c> --description <d>` |
| Label list | `gh label list --repo <r> --limit 1000 --json name,color,description` |
| Auth status | `gh auth status [--hostname <h>]` |
| API user | `gh api user --jq .login` |
| Repo view | `gh repo view --json nameWithOwner,url` |
| API GET | `gh api repos/<r>/<path> [--hostname <h>] [--paginate] [--slurp]` |
| API POST | `gh api repos/<r>/<path> --method POST [-f <field>=<value>] [--input -]` |
| API PATCH | `gh api repos/<r>/<path> --method PATCH -f <field>=<value>` |
| API DELETE | `gh api repos/<r>/<path> --method DELETE` |
| API GraphQL | `gh api graphql -f query=<query> -F <var>=<value> [--hostname <h>]` |
| API JQ | `gh api <endpoint> --jq <filter>` |