# Module: Worktree Safety (Rust Spec)

Source: `internal/worktreesafety/safety.go` (Go)

---

## 1. CheckInput Struct — Tat Ca Fields

```rust
#[derive(Debug, Clone)]
pub struct CheckInput {
    /// Duong dan toi worktree can kiem tra.
    pub worktree_path: String,

    /// Duong dan toi repo git (project repo path).
    pub repo_path: String,

    /// Duong dan root chua worktree (optional).
    /// Khi duoc set, worktree phai nam duoi root nay.
    pub worktree_root: Option<String>,
}
```

Muc dich: Gom cac input can thiet de kiem tra xem mot worktree path co an toan de su dung hay khong.

---

## 2. Validate() — 7-Buoc Kiem Tra

```rust
pub fn validate(input: &CheckInput) -> Result<(), WorktreeSafetyError>
```

### Thu tu thuc hien:

```
Input: CheckInput { worktree_path, repo_path, worktree_root }
Output: Ok(()) hoac Err(WorktreeSafetyError)

1. [Khong empty]     Trim worktree_path, kiem tra khong rong.
                     Neu empty => Err("unsafe worktree path: path is required")

2. [Khac repo path]  samePath(worktree_path, repo_path) == false
                     Neu bang nhau => Err("unsafe worktree path {path}: path must not equal project repo path")

3. [Khac worktree root]  Chi thuc hien neu worktree_root != ""
                     samePath(worktree_path, worktree_root) == false
                     Neu bang nhau => Err("unsafe worktree path {path}: path must not equal worktree root")

4. [Duoi worktree root]  Chi thuc hien neu worktree_root != ""
                     withinRoot(worktree_path, worktree_root) == true
                     Neu khong => Err("unsafe worktree path {path}: path must be under worktree root {root}")

5. [Symlink-aware normalization depth-limited]
                     normalizePath(path) duoc goi, gioi han 255 cap do symlink.
                     normalizePathDepth(path, depth=0):
                       - depth > 255 => fallback ve filepath::Clean(path)
                       - Ap dung resolve relative -> absolute (buoc 6)
                       - Resolve tung component, doc symlink (buoc 7)
                       - De quy khi gap symlink

6. [Resolve tu relative -> absolute]
                     Neu path khong phai absolute:
                       - Lay wd boi os::getcwd()
                       - Ghep: wd + separator + path
                     (Trong Rust: std::fs::canonicalize hoac Path::join)

7. [Resolve ALL symlinks trong path]
                     Duyet tung component cua path:
                       - Neu component la "", "." => skip
                       - Neu component la ".." => current = parent(current)
                       - Neu component ton tai va la symlink:
                           os::readlink(target) -> resolve de quy (depth+1)
                       - Neu component khong ton tai:
                           Clean(candidate + remaining parts) => tra ve
                       - Neu component la directory/file normal:
                           current = candidate

     (Khong co buoc 7 rieng — no nam trong normalizePathDepth loop o buoc 5)
```

### Luu do normalizePathDepth

```
normalizePathDepth(path, depth):
  if depth > 255:
    return filepath::Clean(path)

  // Buoc 6: Resolve relative -> absolute
  abs = path
  if !is_absolute(abs):
    wd = getcwd()
    abs = wd + "/" + path

  // Loai bo volume name (Windows: "C:")
  // Tren Unix: volume = "", rest = abs

  current = "/" (Unix root)
  parts = split(rest, separator)

  for (index, part) in parts:
    match part:
      "" | "." => continue
      ".." => current = parent(current); continue

    candidate = join(current, part)
    info = lstat(candidate)

    if info.error => // path component khong ton tai
      remaining = [candidate] + parts[index+1..]
      return Clean(join_all(remaining))

    if info.is_symlink():
      target = readlink(candidate)
      if is_absolute(target):
        current = normalizePathDepth(target, depth + 1)
      else:
        current = normalizePathDepth(current + "/" + target, depth + 1)
    else:
      current = candidate

  return Clean(current)
```

---

## 3. Cac Ham Phu Tro

### samePath

```rust
fn same_path(a: &str, b: &str) -> bool
```

1. Trim ca a va b.
2. Neu a hoac b empty => false.
3. normalizePath(a) == normalizePath(b).

### withinRoot

```rust
fn within_root(path: &str, root: &str) -> bool
```

1. Trim ca path va root.
2. Neu path hoac root empty => false.
3. normalizedPath = normalizePath(path), normalizedRoot = normalizePath(root).
4. Goi `filepath::Relative(normalizedRoot, normalizedPath)`.
5. Tra ve true neu:
   - rel == "." (chinh la root)
   - Hoac rel != ".." && khong bat bang ".." + separator && khong absolute (tuc la nam trong root).

### normalizePath

```rust
fn normalize_path(path: &str) -> String
```

Wrapper goi `normalizePathDepth(path, 0)`.

---

## 4. Error Types va Messages

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeSafetyError {
    /// Buoc 1: Path empty
    PathRequired,
    /// Buoc 2: Path trung voi repo path
    EqualsRepoPath(String),
    /// Buoc 3 (neu root set): Path trung voi worktree root
    EqualsWorktreeRoot(String),
    /// Buoc 4 (neu root set): Path khong nam duoi worktree root
    OutsideWorktreeRoot { path: String, root: String },
}

impl std::fmt::Display for WorktreeSafetyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorktreeSafetyError::PathRequired =>
                write!(f, "unsafe worktree path: path is required"),
            WorktreeSafetyError::EqualsRepoPath(path) =>
                write!(f, "unsafe worktree path {:?}: path must not equal project repo path", path),
            WorktreeSafetyError::EqualsWorktreeRoot(path) =>
                write!(f, "unsafe worktree path {:?}: path must not equal worktree root", path),
            WorktreeSafetyError::OutsideWorktreeRoot { path, root } =>
                write!(f, "unsafe worktree path {:?}: path must be under worktree root {:?}", path, root),
        }
    }
}
```

### IsSafe

```rust
pub fn is_safe(input: &CheckInput) -> bool {
    validate(input).is_ok()
}
```

---

## 5. Tich Hop voi Git Operations

### Gateways su dung worktreesafety:

| Operation | File | Vi tri Validate | Muc dich |
|-----------|------|-----------------|----------|
| **CreateWorktree** | `gateway.go` | Sau khi `MkdirAll` root, truoc khi `git worktree add` | Dam bao worktreePath an toan |
| **RestoreWorktree** | `gateway.go` | Kiem tra stored worktree path | Dam bao DB record path con hop le |
| **RestoreWorktree (list fallback)** | `gateway.go` | Kiem tra tung candidate tu `git worktree list` | Chi nhan candidate an toan |
| **CleanupWorktree** | `gateway.go` | Truoc khi `git worktree remove --force` | Dam bao khong xoa nham repo path |
| **PrepareWorktree** | `gateway.go` | Qua `validateMutationWorktree()` | Dam bao worktree path an toan truoc fetch/reset |
| **InspectHead** | `gateway.go` | Qua `validateMutationWorktree()` | Dam bao worktree path an toan truoc git operations |
| **Commit** | `gateway.go` | Qua `validateMutationWorktree()` | Dam bao worktree path an toan truoc commit |
| **Push** | `gateway.go` | Qua `validateMutationWorktree()` | Dam bao worktree path an toan truoc push |
| **Planner** | `planner/runner.go` | Kiem tra checkpoint worktree khi resume | Phat hien unsafe path, tao lai worktree |
| **Worker** | `worker/runner.go` | Kiem tra checkpoint worktree khi resume | Phat hien unsafe path, tao lai worktree |
| **Worktree Cleanup** | `worktreecleanup/cleanup.go` | Truoc khi kiem tra worktree status | Phat hien unsafe DB record, bo qua |

### validateMutationWorktree (internal helper)

```go
func (g *Gateway) validateMutationWorktree(worktreePath, repoPath, worktreeRoot string) error {
    if repoPath == "" && worktreeRoot == "" && g.repos != nil {
        return nil  // bypass khi DB co san (fallback safety)
    }
    return worktreesafety.Validate(worktreesafety.CheckInput{
        WorktreePath: worktreePath,
        RepoPath:     repoPath,
        WorktreeRoot: worktreeRoot,
    })
}
```

---

## 6. Protected Branch Validation

**Separate concern** — khong nam trong worktreesafety module, nhung duoc goi cung luc (trong Gateway).

### AssertWritableBranch

```rust
pub fn assert_writable_branch(
    branch: &str,
    protected_branches: &[String],
) -> Result<(), ProtectedBranchError>
```

Logic: Duyet protected_branches; neu `branch == protected` => ProtectedBranchError.

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedBranchError {
    pub branch: String,
}

impl std::fmt::Display for ProtectedBranchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Refusing to modify protected branch: {}", self.branch)
    }
}
```

### Noi goi AssertWritableBranch trong Gateway:

| Method | Protected Branches List |
|--------|----------------------|
| CreateBranch | `append(protectedBranches, startPoint)` |
| CreateWorktree | `append(protectedBranches, baseBranch)` |
| CleanupWorktree | `protectedBranches` (tu input) |
| Push | `protectedBranches` (tu input) |

---

## 7. Test Cases (tu safety_test.go)

| Test | Input | Expected |
|------|-------|----------|
| **Nonexistent child under symlinked root** | worktreePath = `{symlinkRoot}/new-worktree` (chua ton tai), worktreeRoot = `{realRoot}` (symlink target) | **OK** |
| **Sibling prefix outside root** | worktreePath = `{parent}/root-other/wt`, root = `{parent}/root` | **Err** (outside root) |
| **Repo path through symlink** | worktreePath = `{link}` -> `{repoPath}`, repoPath = `{repoPath}` | **Err** (equals repo path) |
| **Existing worktree under symlinked root** | worktreePath = `{symlinkRoot}/existing-wt` (ton tai), root = `{realRoot}` | **OK** |
| **Symlink dot-dot escape** | worktreePath = `{root}/link/../evil-wt`, link -> `{outside}/dir` | **Err** (outside root) |
| **Nested relative symlink dot-dot escape** | worktreePath = `{root}/link`, link (relative) -> `a/../evil-wt`, a -> `{outside}/dir` | **Err** (outside root) |

---

## 8. Canh Bao Khi Porting sang Rust

### Khac biet Go vs Rust:

| Feature | Go | Rust |
|---------|----|------|
| `os.Getwd()` | `os.Getwd()` | `std::env::current_dir()` |
| `os.Lstat()` | `os.Lstat()` | `std::fs::symlink_metadata()` |
| `os.Readlink()` | `os.Readlink()` | `std::fs::read_link()` |
| `filepath.Clean()` | `filepath.Clean()` | `std::path::Path::canonicalize()` (khac: resolve symlink) hoac `Path::components().collect()` |
| `filepath.Rel()` | `filepath.Rel()` | `pathdiff::diff_paths()` hoac `path_abs::PathRel` |
| `filepath.IsAbs()` | `filepath.IsAbs()` | `std::path::Path::is_absolute()` |
| `filepath.VolumeName()` | `filepath.VolumeName()` | Chi can cho Windows, co the dung `std::path::Component::Prefix` |
| `filepath.Join()` | `filepath.Join()` | `std::path::Path::join()` |
| `filepath.Dir()` | `filepath.Dir()` | `std::path::Path::parent()` |

### Ghi chu quan trong:

1. **`filepath.Clean` vs `fs::canonicalize`**: Trong Go, `normalizePath` tu implement symlink resolution ma KHONG dung `filepath.EvalSymlinks` (de xu ly nonexistent paths). Trong Rust, `std::fs::canonicalize` resolve symlinks nhung **yeu cau path phai ton tai**. Vi the can tu viet normalize path logic giong nhu Go.

2. **Depth limit 255**: Can pass depth counter de tranh symlink infinite loop. Go khong dung EvalSymlinks de co quyen kiem soat depth.

3. **Nonexistent path handling**: Khi mot component khong ton tai, Go `normalizePathDepth` dung `filepath.Clean` de ghep phan con lai. Rust can xu ly tuong tu bang `collect` remainder paths.

4. **Path normalization for comparison**: `samePath` dung `normalizePath` de so sanh. Can dam bao macOS `/private/var` vs `/var` symlink duoc xu ly.

5. **`/private` prefix**: Trong `gateway.go`, ham `normalizeComparablePath` bo `/private` prefix cho macOS. worktreesafety `normalizePath` KHONG tu bo `/private`, nhung `samePath` vi no chay qua `filepath.Abs` truoc (tren macOS, `filepath.Abs` khong tu them `/private`). Can kiem tra tren macOS.

### Khuyen nghi crate:

- `std::path::Path` cho path manipulation co ban.
- `path-clean` crate (hoac `sanitize-filename`) cho `filepath::Clean` equivalent.
- Khong dung `fs::canonicalize` — tu viet normalize path de quan ly nonexistent components.
