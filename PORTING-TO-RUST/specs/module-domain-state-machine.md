# Module: Domain State Machine — Behavioral Spec

> Ngôn ngữ: Tiếng Việt (với thuật ngữ kỹ thuật tiếng Anh).
> Mục đích: Đặc tả hành vi (behavioral spec) của domain state machine (loop status, run status, resume policy, step validation, active loop invariant). **Không** phải file-for-file mapping từ Go — đây là đặc tả state machine thuần, tập trung vào WHAT chứ không phải HOW.

---

## 1. LoopType (4 giá trị)

| Constant | String Value |
|---|---|
| `LoopTypePlanner` | `"planner"` |
| `LoopTypeReviewer` | `"reviewer"` |
| `LoopTypeWorker` | `"worker"` |
| `LoopTypeFixer` | `"fixer"` |

### 1.1 LoopTargetType (3 giá trị)

| Constant | String Value |
|---|---|
| `LoopTargetTypeProject` | `"project"` |
| `LoopTargetTypePullRequest` | `"pull_request"` |
| `LoopTargetTypeIssue` | `"issue"` |

### 1.2 LoopType — Target constraint (AssertLoopTypeMatchesTarget)

| LoopType | Allowed TargetTypes |
|---|---|
| `LoopTypePlanner` | `LoopTargetTypeIssue` |
| `LoopTypeReviewer` | `LoopTargetTypePullRequest` |
| `LoopTypeFixer` | `LoopTargetTypePullRequest` |
| `LoopTypeWorker` | `LoopTargetTypeProject` OR `LoopTargetTypePullRequest` OR `LoopTargetTypeIssue` |

Any mismatch returns an error chứa loop type name trong message.

---

## 2. LoopStatus (10 states)

### 2.1 Danh sách đầy đủ

| # | Constant | String Value | Loại |
|---|---|---|---|
| 1 | `LoopStatusIdle` | `"idle"` | Non-terminal, active |
| 2 | `LoopStatusQueued` | `"queued"` | Non-terminal, active |
| 3 | `LoopStatusRunning` | `"running"` | Non-terminal, active |
| 4 | `LoopStatusPaused` | `"paused"` | Non-terminal, active |
| 5 | `LoopStatusWaiting` | `"waiting"` | Non-terminal, active (non-conflicting) |
| 6 | `LoopStatusStopped` | `"stopped"` | Terminal |
| 7 | `LoopStatusTerminated` | `"terminated"` | Terminal |
| 8 | `LoopStatusCompleted` | `"completed"` | Terminal |
| 9 | `LoopStatusFailed` | `"failed"` | Terminal |
| 10 | `LoopStatusInterrupted` | `"interrupted"` | Non-terminal, non-active |

### 2.2 Transition Matrix

| From \ To | queued | running | paused | waiting | stopped | terminated | completed | failed | interrupted |
|---|---|---|---|---|---|---|---|---|---|
| **idle** | YES | - | YES | - | - | YES | - | - | - |
| **queued** | - | YES | YES | - | - | YES | - | - | - |
| **running** | - | - | YES | YES | - | YES | YES | YES | YES |
| **paused** | YES | - | - | - | YES | YES | YES | - | - |
| **waiting** | YES | - | YES | - | YES | YES | - | - | - |
| **stopped** | - | - | - | - | - | - | - | - | - |
| **terminated** | - | - | - | - | - | - | - | - | - |
| **completed** | - | - | - | - | - | - | - | - | - |
| **failed** | - | - | - | - | - | - | - | - | - |
| **interrupted** | YES | - | - | - | - | - | - | YES | - |

**Giải thích chi tiết:**

- **idle** (0 outgoing: 3): Có thể chuyển sang `queued` (bắt đầu), `paused` (hold thủ công), hoặc `terminated` (hủy vĩnh viễn).
- **queued** (3 outgoing): Có thể chuyển sang `running` (scheduler pick), `paused` (hold), hoặc `terminated` (hủy).
- **running** (6 outgoing): Có thể chuyển sang `completed`, `failed`, `paused`, `interrupted`, `waiting`, `terminated`. Đây là state có nhiều outgoing transitions nhất.
- **paused** (4 outgoing): Có thể chuyển sang `queued` (resume), `completed`, `stopped`, `terminated`.
- **waiting** (4 outgoing): Có thể chuyển sang `queued` (resume), `paused`, `stopped`, `terminated`.
- **stopped / terminated / completed / failed**: Terminal states — không có outgoing transitions.
- **interrupted** (2 outgoing): Có thể chuyển sang `queued` (retry) hoặc `failed`. Interrupted không phải terminal.

### 2.3 Terminal States

Các state không có outgoing transitions:

- `LoopStatusStopped`
- `LoopStatusTerminated`
- `LoopStatusCompleted`
- `LoopStatusFailed`

### 2.4 AssertKnownLoopStatus

Hàm validation kiểm tra một string có nằm trong 10 giá trị LoopStatus hay không. Nếu không, trả error listing tất cả 10 giá trị.

---

## 3. RunStatus (7 states)

### 3.1 Danh sách đầy đủ

| # | Constant | String Value | Terminal |
|---|---|---|---|
| 1 | `RunStatusQueued` | `"queued"` | No |
| 2 | `RunStatusRunning` | `"running"` | No |
| 3 | `RunStatusSuccess` | `"success"` | YES |
| 4 | `RunStatusFailed` | `"failed"` | YES |
| 5 | `RunStatusCancelled` | `"cancelled"` | YES |
| 6 | `RunStatusInterrupted` | `"interrupted"` | YES |
| 7 | `RunStatusParseFailed` | `"parse_failed"` | YES |

### 3.2 Transition Matrix

| From \ To | running | success | failed | cancelled | interrupted | parse_failed |
|---|---|---|---|---|---|---|
| **queued** | YES | - | - | - | - | - |
| **running** | - | YES | YES | YES | YES | YES |
| **success** | - | - | - | - | - | - |
| **failed** | - | - | - | - | - | - |
| **cancelled** | - | - | - | - | - | - |
| **interrupted** | - | - | - | - | - | - |
| **parse_failed** | - | - | - | - | - | - |

**Giải thích:**

- **queued** (1 outgoing): Chỉ có thể chuyển sang `running`.
- **running** (5 outgoing): Có thể kết thúc với `success`, `failed`, `cancelled`, `interrupted`, hoặc `parse_failed`.
- **success, failed, cancelled, interrupted, parse_failed**: Terminal — không có outgoing transitions.

### 3.3 AssertRunStatusTransition

Hàm validation kiểm tra `from -> to` có hợp lệ không. Nếu `from` không nằm trong map (unknown status), trả error. Nếu `to` không nằm trong allowed list cho `from`, trả error.

---

## 4. Status Classification Functions

### 4.1 IsActiveLoopStatus

Trả về `true` nếu status thuộc tập **active**:

| Status | IsActive |
|---|---|
| `idle` | YES |
| `queued` | YES |
| `running` | YES |
| `paused` | YES |
| `waiting` | YES |
| `stopped` | No |
| `terminated` | No |
| `completed` | No |
| `failed` | No |
| `interrupted` | No |

**Định nghĩa:** Active loop là loop đang tồn tại trong hệ thống và chưa kết thúc (chưa vào terminal state). Bao gồm cả `idle` (loop vừa được tạo, chưa chạy) và `waiting` (loop đang đợi external event).

### 4.2 IsConflictingActiveLoopStatus

Trả về `true` nếu status thuộc tập **conflicting active**:

| Status | IsConflicting |
|---|---|
| `idle` | YES |
| `queued` | YES |
| `running` | YES |
| `paused` | YES |
| `waiting` | No |
| `stopped` | No |
| `terminated` | No |
| `completed` | No |
| `failed` | No |
| `interrupted` | No |

**Khác biệt với IsActiveLoopStatus:** `waiting` là active nhưng **không** conflicting. Một loop `waiting` không ngăn cản loop khác cùng loại được tạo.

**Rationale:** `waiting` thường là trạng thái loop đang đợi external dependency (ví dụ CI checks). Cho phép tạo loop mới cùng type/target trong khi loop cũ đang waiting.

### 4.3 IsTerminalRunStatus

Trả về `true` nếu run status là terminal:

| Status | IsTerminal |
|---|---|
| `success` | YES |
| `failed` | YES |
| `cancelled` | YES |
| `interrupted` | YES |
| `parse_failed` | YES |
| `queued` | No |
| `running` | No |

Run terminal = run đã kết thúc, không thể transition nữa.

### 4.4 GetActiveStatuses *(khuyến nghị cho Rust port)*

Trả về slice/set chứa 5 active statuses:

```rust
fn get_active_statuses() -> Vec<LoopStatus> {
    vec![
        LoopStatus::Idle,
        LoopStatus::Queued,
        LoopStatus::Running,
        LoopStatus::Paused,
        LoopStatus::Waiting,
    ]
}
```

Hàm này không tồn tại trong Go (chỉ có private map `activeLoopStatuses`), nhưng cần thiết cho Rust port để public API có thể query active loops.

---

## 5. Loop Steps theo LoopType

### 5.1 AssertStepBelongsToLoopType

Mỗi LoopType có một bộ step cố định. Step phải thuộc đúng type của loop hiện tại.

| LoopType | Steps (theo thứ tự) |
|---|---|
| **Planner** | `discover-issues` → `prepare-worktree` → `write-spec` → `publish` → `notify` |
| **Reviewer** | `discover` → `filter` → `claim` → `snapshot` → `review` → `publish` |
| **Worker** | `prepare-work` → `prepare-worktree` → `plan` → `execute` → `validate` → `open-pr` |
| **Fixer** | `discover-pr` → `claim-pr` → `collect-fixes` → `prepare-worktree` → `repair` → `validate` → `push` → `reconcile-commits` → `resolve-comments` → `recheck` |

### 5.2 Step Validation Rules

- `AssertStepBelongsToLoopType(loopType, step)` trả error nếu `loopType` không phải known type.
- Trả error nếu `step` không nằm trong step list của `loopType`.
- Step names là **case-sensitive** string comparison.
- Step list cho mỗi type là **cố định và có thứ tự** (tuần tự, không thể reorder).

### 5.3 Step Semantic

| Step | Mô tả |
|---|---|
| **Planner** |
| `discover-issues` | Parse issue, fetch GitHub detail, check labels, build checkpoint |
| `prepare-worktree` | Tạo git worktree trên branch `looper/planner/{issueNumber}-{slug}` |
| `write-spec` | Agent viết planning spec tại `specs/{YYYY-MM-DD}-{issueNumber}-{slug}.md` |
| `publish` | Push branch, tạo/mở spec PR, add labels/reviewers |
| `notify` | Gửi notification với spec PR URL |
| **Reviewer** |
| `discover` | Fetch PR detail |
| `filter` | Apply scope filters (drafts, labels, review decision, author) |
| `claim` | Acquire lock `pr:{repo}:{number}` |
| `snapshot` | Capture PR state (snapshot checkpoint) |
| `review` | Agent review, submit APPROVE/COMMENT/REQUEST_CHANGES |
| `publish` | Auto-merge nếu allowed, add/remove labels |
| **Worker** |
| `prepare-work` | Parse target (issue/PR), check existing branch/PR để resume |
| `prepare-worktree` | Tạo hoặc restore git worktree |
| `plan` | Agent tạo implementation plan |
| `execute` | Agent implement changes, commit |
| `validate` | Inspect head, verify output |
| `open-pr` | Push, tạo PR, add labels/reviewers |
| **Fixer** |
| `discover-pr` | Fetch PR detail (conflicts, checks) |
| `claim-pr` | Acquire lock `pr:{repo}:{number}` |
| `collect-fixes` | Parse review comments, build FixItem list |
| `prepare-worktree` | Checkout PR branch |
| `repair` | Agent sửa lỗi |
| `validate` | Verify compilation, check remaining issues |
| `push` | Force push with safe-push validation (is-ancestor check) |
| `reconcile-commits` | Commit changes với disclosure-stamped message |
| `resolve-comments` | Resolve hoặc reply review threads |
| `recheck` | Poll CI checks, loop back to repair nếu fail |

---

## 6. LoopTargetKey Computation

`LoopTargetKey(target)` tạo unique key cho một target, dùng để phát hiện conflict loops.

| TargetType | Key Format | Ví dụ |
|---|---|---|
| `LoopTargetTypeProject` | `project:{ProjectID}` | `project:project_abc` |
| `LoopTargetTypeIssue` | `issue:{Repo}:{IssueNumber}` | `issue:acme/looper:42` |
| `LoopTargetTypePullRequest` (default) | `pr:{Repo}:{PRNumber}` | `pr:acme/looper:123` |

**Notes:**

- `TargetType` không hợp lệ fallback về PR format (Go switch default).
- `PRLockKey(repo, prNumber)` là alias: `pr:{repo}:{prNumber}`, dùng riêng cho PR lock. Trả về `""` nếu `repo == ""`.

---

## 7. ResumePolicy (6 giá trị)

### 7.1 Danh sách đầy đủ

| # | Value | Định nghĩa | Dùng bởi |
|---|---|---|---|
| 1 | `"advance_from_checkpoint"` | Tiếp tục từ step kế tiếp sau checkpoint đã lưu. Skip các step đã completed. | Default cho `retryable_after_resume` |
| 2 | `"manual_intervention"` | Dừng hoàn toàn, chờ human action. Không autonomous recovery. | Default cho `manual_intervention` failure |
| 3 | `"replay_step"` | Chạy lại step hiện tại từ đầu. | Default cho mọi failure kind khác |
| 4 | `"restart_from_discover"` | Restart toàn bộ loop từ step discover đầu tiên. Reset checkpoint. | Reviewer `retryable_after_resume` khi PR head changed |
| 5 | `"rerun_review"` | (Reviewer-specific) Chạy lại step review với checkpoint mới. Giữ nguyên các step trước đó. | Reviewer khi review marker bị miss |
| 6 | `"retry_from_timeout_context"` | Retry step hiện tại với agent timeout context từ lần chạy trước. | Planner, Worker, Fixer khi agent timeout |

### 7.2 NormalizeResumePolicy

Khi runner nhận `failureKind` + `resumePolicy`, nếu `resumePolicy` explicit (non-empty) thì giữ nguyên. Nếu empty, chọn default dựa trên `failureKind`:

| failureKind | Default ResumePolicy |
|---|---|
| `"retryable_after_resume"` | `"advance_from_checkpoint"` |
| `"manual_intervention"` | `"manual_intervention"` |
| Mọi failure kind khác (ví dụ `"retryable_transient"`, `"non_retryable"`) | `"replay_step"` |

### 7.3 SuppressesAutonomousRecovery

Trả về `true` nếu policy ngăn autonomous recovery (cần human):

| Condition | Result |
|---|---|
| `resumePolicy == "manual_intervention"` | YES |
| `failureKind == "manual_intervention"` | YES (kể cả resumePolicy empty) |
| Trường hợp khác | No |

### 7.4 ShouldRestartFromDiscover

Chỉ áp dụng khi loop status là `"failed"` hoặc `"interrupted"`. Trả về `true` nếu `resumePolicy == "restart_from_discover"`.

### 7.5 IsManualHoldResumePolicy

Trả về `true` nếu `resumePolicy == "manual_intervention"`.

### 7.6 Resume Policy Lifecycle

```
failure xảy ra
  → classify failure kind
  → runner sets resumePolicy trên checkpoint
  → runtime loop recovery:
       nếu SuppressesAutonomousRecovery → dừng, chờ human
       nếu ShouldRestartFromDiscover → tạo loop mới từ discover
       nếu rerun_review (reviewer) → rerun review step
       nếu retry_from_timeout_context (planner/worker/fixer) → retry step với context cũ
       nếu replay_step → chạy lại step hiện tại
       nếu advance_from_checkpoint → continue từ step kế tiếp
```

---

## 8. Active Loop Invariant: OneActiveLoopPerTypeAndTarget

### 8.1 Định nghĩa

Với mỗi cặp `(ProjectID, LoopType, Target)`, chỉ được có **tối đa một** loop có status là `conflicting active` (idle, queued, running, paused).

### 8.2 AssertUniqueActiveLoop

```
AssertUniqueActiveLoop(existing []LoopSummary, candidate LoopSummary) error
```

**Logic:**

1. Nếu `candidate.Status` **không** phải conflicting active → skip, không conflict.
2. Duyệt từng `loop` trong `existing`:
   a. Nếu `loop.ID == candidate.ID` → skip (cùng loop).
   b. Nếu `loop.Status` không phải conflicting active → skip.
   c. **Ngoại lệ — Concurrent Project Workers:** Nếu cả `loop` và `candidate` đều là `LoopTypeWorker`, và cả hai đều có `TargetType == LoopTargetTypeProject`, và cùng `ProjectID` → cho phép concurrent, skip.
   d. Nếu `loop.ProjectID == candidate.ProjectID` AND `loop.Type == candidate.Type` AND `LoopTargetKey(loop.Target) == LoopTargetKey(candidate.Target)` → **CONFLICT**, trả error.
3. Không tìm thấy conflict → OK.

### 8.3 Exception: Concurrent Project Workers

Worker loops targeting `LoopTargetTypeProject` được phép chạy đồng thời trên cùng project. Điều này cho phép multiple workers xử lý các task khác nhau trên cùng project.

**Ví dụ OK:**
- Worker trên `project:abc` (running) + Worker khác trên `project:abc` (queued) → **allowed** (same project, same type, same project target)

**Ví dụ conflict:**
- Worker trên `issue:acme/looper:42` (running) + Worker khác trên `issue:acme/looper:42` (queued) → **conflict** (cùng target key)
- Reviewer trên `pr:acme/looper:123` (running) + Reviewer khác trên `pr:acme/looper:123` (queued) → **conflict**

### 8.4 Why Waiting is Not Conflicting

`waiting` active nhưng không conflicting. Điều này cho phép:

- Một loop đang `waiting` (ví dụ đợi CI) không block loop mới được tạo.
- Nếu loop cũ là `running` → conflict, không cho tạo mới.
- Nếu loop cũ là `waiting` → cho phép tạo mới (loop cũ sẽ bị reconcile hoặc cancel).

---

## 9. Loop + Run Lifecycle

### 9.1 Mối quan hệ Loop — Run

```
Loop (persistent entity)
  ├── Status máy trạng thái loop (10 states)
  │
  └── Run (ephemeral execution, 1..N per loop)
       ├── Status máy trạng thái run (7 states)
       ├── currentStep: step đang chạy
       ├── lastCompletedStep: step cuối đã hoàn thành
       └── checkpointJSON: state data để resume
```

- Mỗi loop có thể có nhiều runs trong lifecycle.
- Mỗi lần chạy = 1 run mới.
- Run được start khi loop transition `queued → running`.
- Run kết thúc → loop có thể giữ nguyên `running` hoặc transition sang `completed`/`failed`.

### 9.2 Run Start Validation

Khi bắt đầu run mới:

1. Kiểm tra loop đã tồn tại.
2. Kiểm tra loop chưa có running run (HasRunningByLoopID).
3. Nếu loop status chưa phải `running`, validate transition `current_status → running`.
4. Validate `currentStep` thuộc loop type.
5. Validate `lastCompletedStep` thuộc loop type.
6. Upsert run record với status = `"running"`.
7. Force loop status = `"running"`.

### 9.3 Run Completion

Khi kết thúc run:

1. Validate run status transition `current_run_status → target_run_status`.
2. Set `EndedAt`, `LastHeartbeatAt`.
3. Upsert run record.
4. Log event (`run.completed` nếu success, `run.failed` nếu khác).

---

## 10. Failure Kinds (shared enum, 4 giá trị)

| Value | Semantics | Resume Default |
|---|---|---|
| `"retryable_transient"` | Lỗi tạm thời (timeout, lock held). Retry step ngay. | `replay_step` |
| `"retryable_after_resume"` | Lỗi cần resume sau khi fix (PR head changed). | `advance_from_checkpoint` |
| `"non_retryable"` | Lỗi vĩnh viễn (invalid config, missing repo). Không retry. | `replay_step` |
| `"manual_intervention"` | Cần human action. Dừng loop, chờ operator. | `manual_intervention` |

---

## 11. Runtime Recovery Decision Tree

Khi runtime phát hiện loop có failed/interrupted status và muốn quyết định có recover không:

```
runtime_recoverable(failureKind, resumePolicy, queueAttempts, maxAttempts):
  // Hard hold = không recover
  if SuppressesAutonomousRecovery(failureKind, resumePolicy):
    return false

  // restart_from_discover hoặc rerun_review + retryable_after_resume = recover
  if failureKind == "retryable_after_resume" AND
     (resumePolicy == "restart_from_discover" OR resumePolicy == "rerun_review"):
    return true

  // retryable_transient với attempts còn = recover
  if isRetryableTransientWithRemainingAttempts(failureKind, queueAttempts, maxAttempts):
    return true

  // Reviewer-specific guardrail
  if isKnownReviewerRediscoveryGuardrail and isRuntimeReviewerRediscoveryRunStep:
    return true

  // Default = không recover
  return false
```

---

## 12. Key Design Decisions (cho Rust port)

1. **LoopStatus và RunStatus là 2 máy trạng thái riêng biệt, có quan hệ nhưng không lồng nhau.** Loop quản lý lifecycle của automation task. Run quản lý lifecycle của một lần thực thi cụ thể.

2. **Transition validation là pure function** — không side effect, không IO. Input: (from, to). Output: error hoặc nil.

3. **Status classification functions** (`IsActiveLoopStatus`, `IsConflictingActiveLoopStatus`, `IsTerminalRunStatus`) nên được implement như methods trên enum type, không phải free functions.

4. **ResumePolicy normalization** cần failure kind để chọn default. Constructor/builder pattern khuyến khích.

5. **OneActiveLoopPerTypeAndTarget** là invariant ở application layer, không phải storage constraint. Validation cần full list of existing loops làm input.

6. **Step validation** là pure function, chỉ dùng loop type + step name. Step list cố định, compile-time constant.

7. **LoopTargetKey** là deterministic function — không cần IO, chỉ transform input fields.
