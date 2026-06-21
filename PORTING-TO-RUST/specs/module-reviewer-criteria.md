# Module: Reviewer Criteria (Rust Spec)

Source: `internal/reviewer/criteria/` (Go)

---

## 1. Kieu Enum va Struct chinh

### AcceptanceCriterion

```rust
pub type AcceptanceCriterion = String;
```

Mot chuoi string dai dien cho mot tieu chi chap nhan duoc trich xuat tu issue body.

### Verdict

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    Unverifiable,
}
```

- **Pass**: Diff chua bang chung khop voi criterion.
- **Fail**: Diff chua bang chung phu dinh criterion (khong co trong DefaultVerifier, nhung Verifier tuy chinh co the tra ve).
- **Unverifiable**: Khong tim thay bang chung xac dinh trong diff (mac dinh khi khong khop).

### AggregateDisposition

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggregateDisposition {
    Pass,
    Fail,
    Unverifiable,
}
```

Ket qua tong hop sau khi Verify() xu ly tat ca criteria.

### Evidence

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub file_path: String,   // duong dan file trong diff
    pub start_line: usize,   // dong bat dau (1-based)
    pub end_line: usize,     // dong ket thuc (1-based, >= start_line)
}
```

### CriterionAssessment

```rust
#[derive(Debug, Clone)]
pub struct CriterionAssessment {
    pub verdict: Verdict,
    pub justification: String,
    pub evidence: Vec<Evidence>,
}
```

Tra ve boi `Verifier::verify_criterion()`.

### CriterionResult

```rust
#[derive(Debug, Clone)]
pub struct CriterionResult {
    pub criterion: AcceptanceCriterion,
    pub verdict: Verdict,
    pub justification: String,
    pub evidence: Vec<Evidence>,
}
```

Mot criterion da duoc danh gia, chua trong `VerificationResult`.

### VerificationResult

```rust
#[derive(Debug, Clone)]
pub struct VerificationResult {
    pub disposition: AggregateDisposition,
    pub criteria: Vec<CriterionResult>,
}
```

### PRDiff va DiffFile

```rust
#[derive(Debug, Clone)]
pub struct PRDiff {
    pub files: Vec<DiffFile>,
}

#[derive(Debug, Clone)]
pub struct DiffFile {
    pub path: String,
    pub patch: String,
}
```

### Verifier trait

```rust
pub trait Verifier {
    fn verify_criterion(
        &self,
        criterion: &AcceptanceCriterion,
        diff: &PRDiff,
    ) -> Result<CriterionAssessment, Box<dyn std::error::Error>>;
}
```

---

## 2. DefaultVerifier

### Struct va Constructor

```rust
pub struct DefaultVerifier;

impl DefaultVerifier {
    pub fn new() -> Self {
        DefaultVerifier
    }
}
```

Khong co fields — stateless. Co the tao bang `DefaultVerifier::new()` hoac `DefaultVerifier`.

### VerifyCriterion() — Logic Flow

```
Input: criterion (str), diff (PRDiff)
Output: CriterionAssessment

1. Normalize criterion: trim, lowercase
2. Tach criterion thanh tokens (chuoi alphanumeric dai >= 4 ky tu)
3. Collect tat ca added lines tu diff:
   - Doc tung file.Patch
   - Parse @@ hunk header de lay line number
   - Lay cac dong bat dau bang "+" (khong tinh "+++")
   - Bo qua cac dong "-" (xoa)
   - Context lines (" ") khong phai added
4. Voi moi added line, kiem tra:
   a. Neu lineLower CHUA criterionText (substring match): => Pass
   b. Neu tokenOverlap(tokens, lineTokens) >= min(2, len(criterionTokens)): => Pass
      - tokenOverlap match DISTINCT: moi token chi duoc dem 1 lan
      - Yeu cau criterion co it nhat 2 tokens de overlap co hieu luc
5. Neu khong co dong nao khop: => Unverifiable
```

### Token Heuristic

```rust
fn criterion_tokens(value: &str) -> Vec<String> {
    // Tach boi non-alphanumeric, chi giu token >= 4 ky tu
}

fn token_overlap(a: &[String], b: &[String]) -> bool {
    if a.len() < 2 || b.is_empty() { return false; }
    // Dem distinct matches
    // Tra ve true neu matches >= min(2, a.len())
}
```

### Hunk Start Parser

```rust
fn hunk_start(raw: &str) -> usize {
    // Parse hunk header @@ -a,b +c,d @@
    // Tra ve c (new file start line)
    // Bo qua cac phan phuc tap (comma, offset)
}
```

### Vi du:

- Criterion `"add tests"` → tokens `["tests"]` (1 token) → can khong du overlap, chi pass neu exact match.
- Criterion `"update tests docs"` → tokens `["update", "tests", "docs"]` (3 tokens) → can 2 distinct overlap.

### Test Cases (default_verifier_test.go)

| Test | Criterion | Added Line | Expected |
|------|-----------|-----------|----------|
| Single-token criterion chi exact match | "add tests" | "tests tests cover another path" | Unverifiable (chi 1 token "tests", nhung overlap can >= 2) |
| Exact match override | "add tests" | "add tests for reviewer auto-merge" | Pass (exact substring) |
| Overlap can distinct tokens | "update tests docs" | "tests tests cover another path" | Unverifiable (chi 1 distinct token "tests") |

---

## 3. Extract() — Trich xuat Acceptance Criteria tu Issue Body

```rust
pub fn extract(issue_body: &str) -> Vec<AcceptanceCriterion>
```

### Logic:

1. Split issue_body thanh tung dong.
2. Duyet qua tung dong, bo qua cho den khi gap heading khop voi "Acceptance Criteria".
   - Nhan dien heading: bat dau bang `#`, level 1-6, theo sau boi space.
   - Kiem tra heading text (trim, bo `#`, `:`, `;`, `.`, `!`, `?`) — case-insensitive.
   - Chi khop: `"Acceptance Criteria"`, `"Acceptance Criteria:"`, `"Acceptance Criteria ###"`, `"## Acceptance criteria ##"`.
3. Khi vao section, doc cac dong cho den khi:
   - Gap heading cung level hoac cao hon → stop parsing.
   - Hoac gap heading level thap hon → tiep tuc (nested headings duoc phep).
4. Moi dong duoc parse nhu criterion neu:
   - Bat dau bang `- ` hoac `* ` (list item).
   - Trim checkbox prefix: `[ ]`, `[x]`, `[X]`, `[]` (optional).
   - Phan con lai la criterion text.
5. Tra ve `Vec<AcceptanceCriterion>`.

### Test Cases:

| Issue Body | Extracted Criteria |
|-----------|-------------------|
| `"## Acceptance criteria\n- [ ] first criterion\n- [x] second criterion"` | `["first criterion", "second criterion"]` |
| `"## Acceptance Criteria:\n- [ ] first"` | `["first"]` |
| `"### Acceptance Criteria\n- [ ] first"` | `["first"]` |
| `"## Acceptance Criteria ##\n- [ ] first"` | `["first"]` |
| `"## Acceptance Criteria\n- [ ] api\n\n### Backend\n- [ ] worker\n\n## Notes"` | `["api", "worker"]` |
| `"## Summary\n- [ ] not here"` | `[]` |
| `"## Acceptance criteria\n- [] missing space\n* [x] valid\n- no checkbox"` | `["missing space", "valid", "no checkbox"]` |
| `"## Acceptance criteria\n- [ ] [Spec](https://ex.com) done"` | `["[Spec](https://ex.com) done"]` |

---

## 4. Verify() — Complete Orchestration

```rust
pub fn verify(
    criteria: &[AcceptanceCriterion],
    diff: &PRDiff,
    verifier: Option<&dyn Verifier>,
) -> Result<VerificationResult, Box<dyn std::error::Error>>
```

### Logic Flow:

1. **Guard**: Neu `verifier` la None va criteria khong rong => tra ve Err.
2. **Iterate**: Voi moi criterion:
   a. Goi `verifier.verify_criterion(criterion, diff)`.
   b. **validateAssessment**: Kiem tra:
      - Verdict phai la mot trong 3 gia tri hop le (Pass, Fail, Unverifiable).
      - Justification khong duoc empty (trim check).
      - Neu Pass: Evidence khong duoc rong.
      - Moi Evidence phai co:
        - FilePath khong empty.
        - StartLine >= 1.
        - EndLine >= StartLine.
        - **diffContainsEvidence**: Evidence phai nam trong diff (dung diffanchor de validate rang evidence co the tim thay trong patch hunk).
   c. Neu loi validate => tra ve Err ngay.
   d. Neu OK => append CriterionResult (deep copy evidence).
3. **Aggregate disposition** (how criteria combine):
   - Khoi tao = `DispositionPass`.
   - Voi moi criterion:
     - `Verdict::Fail` => disposition = `DispositionFail` (override moi thu).
     - `Verdict::Unverifiable` => chi set disposition = `DispositionUnverifiable` neu hien tai chua phai Fail.
   - **Thu tu uu tien**: Fail > Unverifiable > Pass.
4. Tra ve `VerificationResult { disposition, criteria }`.

### diffContainsEvidence

```rust
fn diff_contains_evidence(diff: &PRDiff, evidence: &Evidence) -> bool
```

Su dung `diffanchor` module de parse patch va kiem tra:

1. Tim file trong diff khop evidence.FilePath.
2. Ghep lai patch thanh `diff --git ...` format.
3. Dung `parsed.validate(anchor)` voi:
   - Path = evidence.FilePath
   - StartLine = evidence.StartLine
   - StartSide = Right
   - Line = evidence.EndLine
   - Side = Right
4. Tra ve `valid == true`.

---

## 5. Cach Verdict Map to Review Event

### Config: ReviewerReviewEventsConfig

```rust
pub enum ReviewerReviewEvent {
    Comment,       // "COMMENT"
    Approve,       // "APPROVE"
    RequestChanges, // "REQUEST_CHANGES"
}

pub struct ReviewerReviewEventsConfig {
    pub clean: ReviewerReviewEvent,    // Mac dinh: Comment
    pub blocking: ReviewerReviewEvent, // Mac dinh: Comment
}
```

### Mapping Khi Publish Review

Logic trong runner.go (ham maybePublishCriteriaAnchoredCleanReview, publishCriteriaApprovedReview, publishCriteriaFailureReview):

| Tinh huong | Event Duoc Submit |
|-----------|------------------|
| **Clean review** (khong co issue linked / khong criteria) + `clean == Approve` | `APPROVE` |
| **Clean review** + `clean == Comment` | No review submit |
| **All criteria pass** | `APPROVE` (sau do goi automerge) |
| **Criteria fail** (disposition = Fail hoac Unverifiable) | `COMMENT` (non-blocking) |
| **Blocking event** cho fail criteria | Khong ap dung automerge, chi comment |
| **Blocking policy** `blocking == RequestChanges` + Fail | `REQUEST_CHANGES` (qua ReviewEventRequestChanges) |
| **Agent-native clean** (noop summary) | `AGENT_NATIVE` (marker, khong submit review API) |

### Constraints:

- `clean` chi co the la `Comment` hoac `Approve` (khong the `RequestChanges`).
- `blocking` chi co the la `Comment` hoac `RequestChanges` (khong the `Approve`).
- Neu agent tu approve PR cua chinh no (`selfApprovalFallback`): `APPROVE` → `COMMENT`.

---

## 6. AutoMergeConfig va ShouldEnableAutoMerge()

### ReviewerAutoMergeConfig

```rust
pub struct ReviewerAutoMergeConfig {
    pub enabled: bool,                    // mac dinh: false
    pub strategy: ReviewerAutoMergeStrategy, // Squash, Merge, Rebase
    pub require_branch_protection: bool,  // mac dinh: true
    pub transient_retries: usize,         // mac dinh: 3
    pub scope: ReviewerAutoMergeScope,    // LooperOnly
}
```

### ReviewerAutoMergeStrategy

```rust
pub enum ReviewerAutoMergeStrategy {
    Squash, // "squash"
    Merge,  // "merge"
    Rebase, // "rebase"
}
```

### ReviewerAutoMergeScope

```rust
pub enum ReviewerAutoMergeScope {
    LooperOnly, // "looper-only"
}
```

### AutoMergeDecision

```rust
pub struct AutoMergeDecision {
    pub strategy: Option<ReviewerAutoMergeStrategy>,
    pub reason: Option<RefusalReason>,
}

pub enum RefusalReason {
    Disabled,
    Scope,
    NoBranchProtection,
    StrategyDisallowed,
    AutoMergeDisabled,
}
```

### PRSnapshot, BranchProtectionSnapshot, RepoSettingsSnapshot

```rust
pub struct PRSnapshot {
    pub labels: Vec<String>,
    pub has_tracked_issue_link: bool,
}

pub struct BranchProtectionSnapshot {
    pub exists: bool,
    pub has_required_checks: bool,
}

pub struct RepoSettingsSnapshot {
    pub allow_squash_merge: bool,
    pub allow_merge_commit: bool,
    pub allow_rebase_merge: bool,
    pub allow_auto_merge: bool,
}
```

### Decide() — ShouldEnableAutoMerge Conditions

```
Input: PRSnapshot, ReviewerAutoMergeConfig, BranchProtectionSnapshot, RepoSettingsSnapshot
Output: AutoMergeDecision

Condition sequence (check all, return first failure):

1. [Enabled]  autoMergeConfig.enabled == true         -> RefusalReason::Disabled
2. [Scope]    hasLooperLabel(pr.labels) == true        -> RefusalReason::Scope
              && pr.has_tracked_issue_link == true
3. [Protection] autoMergeConfig.require_branch_protection == false
               || (protection.exists && protection.has_required_checks)
                                                         -> RefusalReason::NoBranchProtection
4. [Strategy] StrategyAllowed(strategy, settings) == true -> RefusalReason::StrategyDisallowed
              (kiem tra settings allow merge type tuong ung)
5. [Settings] settings.allow_auto_merge == true          -> RefusalReason::AutoMergeDisabled
6. [Pass]     => OptInWithStrategy(strategy)
```

`hasLooperLabel`: kiem tra bat ky label nao co tien to "looper:" (case-insensitive).

`StrategyAllowed`:
- Squash → `settings.allow_squash_merge`
- Merge → `settings.allow_merge_commit`
- Rebase → `settings.allow_rebase_merge`

### Khi nao goi EnableAutoMerge?

Trong `publishCriteriaApprovedReview`:

```
1. Submit APPROVE review.
2. Neu submit thanh cong (marker.Event == Approve):
   a. Goi Decide()
   b. Neu decision.reason == None:
      - Goi GitHub API EnableAutoMerge voi strategy.
   c. Neu decision.reason != None:
      - Post comment "Auto-merge opt-in was refused for this PR: {reason}."
```

---

## 7. Branch Protection + Checks Validation

### GitHub Gateway Methods

```rust
// Lay repository settings (merge types, auto-merge enable)
pub fn get_repository_settings(ctx, repo) -> Result<RepoSettingsSnapshot>;

// Lay branch protection status
pub fn get_branch_protection(ctx, repo, branch) -> Result<BranchProtectionSnapshot>;

// Enable auto-merge tren PR
pub fn enable_auto_merge(ctx, repo, pr_number, strategy, head_sha) -> Result<()>;
```

### Khi RequireBranchProtection == true:

`get_branch_protection` duoc goi voi branch = `detail.base_ref_name` (hoac "main" neu khong co).

Kiem tra:
- `protection.enabled` (branch protection co ton tai khong)
- `protection.has_required_checks` (co status check nao required khong)

Neu thieu => `RefusalReason::NoBranchProtection`.

### TransientRetries

```
config.transient_retries: mac dinh 3
- So lan retry khi goi GitHub API EnableAutoMerge bi loi transient.
```

---

## 8. Design Patterns va Extension Points

### 1. Verifier Trait (Strategy Pattern)

```rust
pub trait Verifier {
    fn verify_criterion(
        &self,
        criterion: &AcceptanceCriterion,
        diff: &PRDiff,
    ) -> Result<CriterionAssessment, Box<dyn std::error::Error>>;
}
```

- **DefaultVerifier**: text-matching heuristic (substring + token overlap).
- **Co the mo rong**: Verifier khac su dung LLM, code analysis, AST diff, etc.
- Runner goi: `let verifier = options.criteria_verifier.unwrap_or(DefaultVerifier::new())`.

### 2. Verifier Composition (Future)

```rust
pub struct CompositeVerifier {
    verifiers: Vec<Box<dyn Verifier>>,
}
// Chay tung verifier, lay ket qua "best" (pass > unverifiable > fail)
```

### 3. Criteria Extraction Heuristic

`extract()` doc duoc:
- Heading `Acceptance Criteria` o bat ky level nao (##, ###, v.v.).
- Nested headings trong section (cho phep sub-sections).
- Markdown checkbox syntax: `- [ ]`, `- [x]`, `* [ ]`, `* [x]`.

### 4. Chaining with AutoMerge

- Verify() chi la 1 buoc trong publish pipeline.
- Pipeline: `maybePublishCriteriaAnchoredCleanReview`
  → `resolveLinkedIssueForCriteria` (tim issue linked)
  → `extract` (trich criteria)
  → `verify` (kiem tra)
  → `publishCriteriaApprovedReview` (approve + automerge)
  → `publishCriteriaFailureReview` (comment + remove triage labels)

### 5. Evidence Validation

- `validateAssessment()` dam bao:
  - Pass phai kem evidence.
  - Evidence phai nam trong diff (khong duoc reference file/dong khong co trong PR).
  - Su dung `diffanchor` module de parse diff hunk va xac minh.
- Day la invariant: Verifier custom cung phai tuan thu.

### 6. Marker Heuristic (Idempotency)

- Moi review body chua marker: `<!-- looper:reviewer:marker {loop_id} {head_sha} {idempotency_key} -->`
- `verifyAgentNativeReviewMarker()` kiem tra marker co ton tai khong.
- Tranh double-submit.

### 7. Criteria Failure Side Effects

Khi criteria fail:
- Submit `COMMENT` review (non-blocking).
- Remove triage labels: "triaged", "dispatch/*".
- Remove +1 reaction tu clean review truoc do.
- Issue quay lai trang thai can re-triage.

---

## 9. Toan bo Constants va Magic Values

```rust
// Markers trong review body
pub const CRITERIA_FAIL_COMMENT_MARKER: &str = "<!-- looper:reviewer:criteria-fail -->";
pub const AUTOMERGE_REFUSED_COMMENT_MARKER: &str = "<!-- looper:reviewer:automerge-refused -->";
pub const CRITERIA_VERIFICATION_HEADING: &str = "### Acceptance criteria verification";

// Token heuristic
pub const MIN_TOKEN_LENGTH: usize = 4;
pub const MAX_SYMLINK_DEPTH: usize = 255; // shared voi worktree safety

// Defaults
pub const DEFAULT_TRANSIENT_RETRIES: usize = 3;
pub const DEFAULT_STRATEGY: ReviewerAutoMergeStrategy = ReviewerAutoMergeStrategy::Squash;
```
