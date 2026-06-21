# Module 4: looper-git — Worktree Management (Rust Spec)

Source: `~/Projects/looper/internal/infra/git/gateway.go` (1169 lines)

---

## 1. Public Types

### Gateway Struct
```go
type Gateway struct {
    gitPath string        // resolves via Options.GitPath or defaults to "git"
    repos   *storage.Repositories  // optional, for DB persistence
    now     func() time.Time
}
```
Constructor: `func New(options Options) *Gateway`

### Options
```go
type Options struct {
    GitPath string
    Repos   *storage.Repositories
    Now     func() time.Time
}
```

### CheckoutMode
```go
type CheckoutMode string
const CheckoutModeBranch   CheckoutMode = "branch"
const CheckoutModeDetached CheckoutMode = "detached"
```

---

## 2. ALL Public Function Signatures

| Function | Input | Returns | Description |
|----------|-------|---------|-------------|
| `CreateWorktree` | `CreateWorktreeInput` | `(storage.WorktreeRecord, error)` | Create worktree, restore from DB if possible |
| `RestoreWorktree` | `RestoreWorktreeInput` | `(*storage.WorktreeRecord, error)` | Restore existing worktree from DB |
| `CleanupWorktree` | `CleanupWorktreeInput` | `error` | Remove worktree and mark "cleaned" in DB |
| `ListWorktrees` | `ctx, repoPath` | `([]WorktreeListEntry, error)` | List worktrees via `git worktree list --porcelain` |
| `WorktreeClean` | `ctx, worktreePath` | `(bool, error)` | Check if worktree has uncommitted changes |
| `IsWorktreeClean` | `ctx, worktreePath` | `(bool, error)` | Alias for WorktreeClean |
| `PrepareWorktree` | `PrepareWorktreeInput` | `(PrepareWorktreeResult, error)` | Fetch + reset worktree to match remote |
| `InspectHead` | `InspectHeadInput` | `(InspectHeadResult, error)` | Get HEAD sha, new commits, changed files |
| `Commit` | `CommitInput` | `(CommitResult, error)` | `git add -A` + `git commit -m` in worktree |
| `Push` | `PushInput` | `error` | Push with `--force-with-lease` or simple `push -u` |
| `CreateBranch` | `ctx, repoPath, branch, startPoint, protectedBranches` | `error` | `git branch --force` |
| `DetectGitHubRepo` | `ctx, repoPath` | `(string, error)` | Parse remote origin URL for GitHub repo |
| `FetchBranch` | `ctx, repoPath, remote, branch` | `error` | `git fetch <remote> <branch>` |
| `IsAncestor` | `ctx, repoPath, ancestor, descendant` | `(bool, error)` | `git merge-base --is-ancestor` |
| `AssertWritableBranch` | `branch, protectedBranches` | `error` | Check branch not in protected list |

---

## 3. ALL Git Operations — Exact Command Lines

### Create Worktree
```
# Detached mode (PR review):
git worktree add --force --detach <worktreePath> <startPoint>

# Branch mode, branch exists:
git worktree add --force <worktreePath> <branch>

# Branch mode, new branch:
git worktree add --force -b <branch> <worktreePath> <startPoint>
```

### Remove / Cleanup Worktree
```
git worktree remove --force <worktreePath>
```

### List Worktrees
```
git worktree list --porcelain
```

### Prepare Worktree (fetch + reset)
```
git fetch <remote> <targetSpec>
git reset --hard <resetRef>
```

### Inspect Head
```
git rev-parse HEAD                          # get current SHA
git rev-list --reverse <baseRef>..HEAD      # new commits since base
git status --porcelain --untracked-files=all --ignored=no   # changed files
```

### Commit
```
git add -A
git commit -m <message>
```

### Push (with lease)
```
git push --porcelain --force-with-lease=refs/heads/<branch>:<expectedSHA> -u <remote> HEAD:refs/heads/<branch>
```
Or without lease check:
```
git push -u <remote> HEAD:refs/heads/<branch>
```

### Create Branch
```
git branch --force <branch> <startPoint>
```

### Branch Exists Check
```
git show-ref --quiet --verify refs/heads/<branch>
```
Returns exit code 0 = exists, 1 = does not exist.

### Remote Branch Exists
```
git show-ref --quiet --verify refs/remotes/<remote>/<branch>
```

### Detached Check
```
git rev-parse --abbrev-ref HEAD   # returns "HEAD" when detached
```

### Current Branch
```
git rev-parse --abbrev-ref HEAD
```

### Health Check
```
git status --porcelain --untracked-files=all   # runs inside worktree
```

### Has Remote
```
git config --get remote.<remote>.url
```

### Fetch Ref (detached resolve)
```
git fetch <remote> +refs/heads/<branch>:refs/remotes/<remote>/<branch>
```

### Remote Head SHA
```
git ls-remote --heads <remote> <branch>
```

### Detect GitHub Repo
```
git config --get remote.origin.url
```

### Is Ancestor
```
git merge-base --is-ancestor <ancestor> <descendant>
```

### Get Revision / SHA
```
git rev-parse <ref>
```

---

## 4. Worktree Lifecycle

### 4.1 Create
1. Validate branch is writable (`AssertWritableBranch`)
2. Create worktree root directory (`os.MkdirAll`)
3. Compute worktree path: `filepath.Join(worktreeRoot, buildWorktreeDirectoryName(input))`
4. Validate path safety via `worktreesafety.Validate()`
5. Attempt `RestoreWorktree` — if restored from DB record, return immediately
6. Create worktree:
   - Detached: `git worktree add --force --detach <path> <startPoint>`
   - Branch exists: `git worktree add --force <path> <branch>`
   - New branch: `git worktree add --force -b <branch> <path> <startPoint>`
7. Get HEAD SHA via `git rev-parse HEAD`
8. Upsert `storage.WorktreeRecord` in DB (ID, ProjectID, RepoPath, WorktreePath, Branch, BaseBranch, Status="active", HeadSHA, MetadataJSON, CreatedAt, UpdatedAt)

### 4.2 Restore (lookup before create)
1. Look up existing DB record by project + branch
2. If found and not "cleaned", repo path matches, and path is safe:
   - Check health via `isHealthyWorktree` (os.Stat + git status)
   - If unhealthy: remove and continue
   - If checkout mode matches: update head SHA, status="active", upsert DB
   - If mismatch: decide replace, remove if so
3. Scan `git worktree list --porcelain` for matching worktree
4. If found: verify health + checkout mode; remove if unhealthy/bad mode
5. Create new DB record (MetadataJSON `{"recovered":true}`)

### 4.3 Cleanup (remove)
1. Validate branch is writable
2. Validate path safety
3. Run `git worktree remove --force <worktreePath>`
4. If error matches missing worktree pattern → ignore (already gone)
5. Upsert DB record: Status="cleaned", CleanedAt=now, UpdatedAt=now

### 4.4 Prepare (sync to remote)
1. Validate path safety
2. Default remote to "origin"
3. Fetch: `git fetch <remote> <targetSpec>`
4. Check remote head SHA against expected
5. If clean status: `git reset --hard <resetRef>` if local != remote
6. Report head SHA and dirty status

---

## 5. Branch Naming Conventions

### Directory Name Generation (`buildWorktreeDirectoryName`)

```go
func buildWorktreeDirectoryName(input CreateWorktreeInput) string {
    if input.PRNumber != 0 {
        if normalizeCheckoutMode(input.CheckoutMode) == CheckoutModeDetached {
            return fmt.Sprintf("looper-fix-%s-pr-%d-detached", sanitizeBranchName(input.ProjectID), input.PRNumber)
        }
        return fmt.Sprintf("looper-fix-%s-pr-%d", sanitizeBranchName(input.ProjectID), input.PRNumber)
    }
    return sanitizeBranchName(input.Branch)
}
```

### `sanitizeBranchName`
- Keeps: `[a-zA-Z0-9._-]`
- Replaces everything else with `-`

### Examples
- Branch `feature/my-thing` → `feature-my-thing`
- Branch `fix/JIRA-123_bug` → `fix-JIRA-123_bug`
- PR review (detached): `looper-fix-<project>-pr-<N>-detached`
- PR review (branch): `looper-fix-<project>-pr-<N>`

---

## 6. Safety Guards

### Protected Branch Protection
`AssertWritableBranch(branch, protectedBranches []string) error`
- Returns `&ProtectedBranchError{Branch}` if branch is in protected list
- Used in: `CreateBranch`, `CreateWorktree`, `CleanupWorktree`, `Push`
- Base branch and starting point are automatically added to protected list

### Worktree Path Safety (`worktreesafety` package)
`Validate(input CheckInput) error` checks:
1. Path must not be empty
2. Path must not equal repo path
3. If worktree root is set: path must not equal root, path must be under root
4. Symlink-aware path normalization (recursive, depth-limited to 255)
5. Resolves relative paths to absolute via `os.Getwd()`
6. Resolves all symlinks in the path

### Remote Head Changed Detection (`Push`)
- Uses `--force-with-lease=refs/heads/<branch>:<expectedSHA>`
- Before push: verifies local HEAD descends from expected SHA via `git merge-base --is-ancestor`
- On push conflict: looks up actual remote head via `git ls-remote --heads`
- Returns `&RemoteHeadChangedError{Branch, ExpectedHeadSHA, ActualHeadSHA}`

### Mutation Safety (`validateMutationWorktree`)
- Legacy callers without repo/worktree root context: no check
- With context: full `worktreesafety.Validate` check

### Uncommitted Changes Guard (`PrepareWorktree`)
- If status is dirty before reset → returns `Clean: false` without resetting
- Only does `git reset --hard` if working directory is clean

### Detached Worktree Check on Restore
- For detached mode: checks `git rev-parse --abbrev-ref HEAD` == "HEAD"
- For branch mode: checks current branch matches expected

---

## 7. Lock Files & Concurrent Access

### Retry on Fetch Ref Lock Race
```go
var fetchRefLockRetryDelays = []time.Duration{50 * time.Millisecond, 100 * time.Millisecond}
```
- `runGitResult` retries up to 2 additional attempts (3 total) when:
  - Command starts with "fetch" AND error message contains both `"cannot lock ref"` and `" but expected "`
- Retry is context-aware (respects ctx cancellation)
- Only retries fetch commands — other git commands fail immediately

### No other explicit lock handling
- The code does NOT use file-level lock files (.lock) for concurrent git access
- Relies on git's own internal lock mechanisms
- DB-level concurrency handled via SQLite in `storage.Repositories`

---

## 8. Error Handling Patterns

### Shell Integration
All git commands pass through `shell.Run(ctx, shell.Options{Command: gitPath, Args: args, CWD: cwd, Env: env})`

Error wrapping format:
```
git <args>: <stderr>
```
Wraps in `shell.CommandExecutionError` for structured exit code access.

### Error Pattern Matching
```go
missingWorktreeErrorPattern = regexp.MustCompile(`(?i)is not a working tree|does not exist|not found|no such file`)
pushConflictErrorPattern    = regexp.MustCompile(`(?i)stale info|non-fast-forward|failed to push|rejected`)
```

### Exit Code Parsing
- `git show-ref --quiet` exit code 1 → false (not exists)
- `git config --get` exit code 1 → false (no remote)
- `git merge-base --is-ancestor` exit code 1 → false

---

## 9. WorktreeRecord Storage Type
```go
type WorktreeRecord struct {
    ID           string
    ProjectID    string
    RepoPath     string
    WorktreePath string
    Branch       string
    BaseBranch   *string
    Status       string     // "active" or "cleaned"
    HeadSHA      *string
    MetadataJSON *string
    CreatedAt    string     // ISO format: "2006-01-02T15:04:05.000Z"
    UpdatedAt    string
    CleanedAt    *string
}
```

---

## 10. WorktreeListEntry (from `git worktree list --porcelain`)
```go
type WorktreeListEntry struct {
    Path    string   // "worktree /path/to/worktree"
    Branch  string   // "branch refs/heads/foo" → "foo"
    HeadSHA string   // "HEAD abc123..."
    Bare    bool     // "bare"
}
```

---

## 11. Branch Existence Resolution Logic

### `resolveDetachedStartPointRef` (ordered lookup)
1. Check if remote "origin" exists
2. If yes: try `git fetch +refs/heads/<branch>:refs/remotes/origin/<branch>`
3. Check `git show-ref --quiet refs/remotes/origin/<branch>` → return `origin/<branch>`
4. Check `git show-ref --quiet refs/heads/<branch>` → return `<branch>`
5. Return empty if neither found

### `resolveAttachedStartPoint`
1. Try remote branch first (same as detached)
2. If not found: check if baseBranch exists locally → use it
3. Error if nothing resolves

---

## 12. Worktree Cleanup (Daemon-Controlled)

File: `~/Projects/looper/internal/runtime/worktree_cleanup.go`

- Background loop: initial 1min delay, then every 1 hour (configurable)
- Uses `worktreecleanup.Service.Plan()` to find candidates
- Cleanup criteria:
  - Worktree must be past retention window (configurable `RetentionDays`)
  - No active loop (idle/queued/running/paused/waiting/failed/interrupted) references it
  - No running run references it
  - No active queue item references it
  - Project must exist and not be archived
  - Worktree must still exist on disk AND be in `git worktree list`
  - Must be clean (no uncommitted changes)
- Safety: `worktreesafety.Validate()` before any mutation
- Dry-run mode available
- MaxPerTick (default 10) limits cleanup per pass
- Respects `ctx.Err()` for graceful shutdown mid-pass
