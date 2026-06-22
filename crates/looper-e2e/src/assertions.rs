use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::git::RepoSnapshot;

/// Evidence of CWD, args, env, and process metadata captured by the
/// fake-agent binary during an E2E test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CWDEvidence {
    /// The working directory observed by the fake agent.
    pub cwd: String,
    /// Command-line arguments passed to the fake agent.
    pub args: Vec<String>,
    /// Environment variables seen by the fake agent.
    pub env: HashMap<String, String>,
    /// ISO-8601 timestamp when the evidence was captured.
    pub timestamp: String,
    /// Mode string (e.g. "cwd-evidence").
    pub mode: String,
    /// Process ID.
    pub pid: u32,
}

/// Deserialise a [`CWDEvidence`] from a JSON file at `path`.
///
/// # Panics
/// Panics if the file cannot be read or decoded.
pub fn load_cwd_evidence(path: impl AsRef<Path>) -> CWDEvidence {
    let payload = std::fs::read_to_string(path.as_ref())
        .unwrap_or_else(|e| panic!("read cwd evidence at {:?}: {}", path.as_ref(), e));
    serde_json::from_str(&payload)
        .unwrap_or_else(|e| panic!("decode cwd evidence at {:?}: {}", path.as_ref(), e))
}

/// Assert that two [`RepoSnapshot`] values are identical (head, status, index tree).
///
/// # Panics
/// Panics if any field differs.
pub fn assert_repo_unchanged(before: &RepoSnapshot, after: &RepoSnapshot) {
    assert_eq!(
        before.head, after.head,
        "repo head changed: before={} after={}",
        before.head, after.head
    );
    assert_eq!(
        before.status_porcelain, after.status_porcelain,
        "repo status changed:\nbefore:\n{}\nafter:\n{}",
        before.status_porcelain, after.status_porcelain
    );
    assert_eq!(
        before.index_tree, after.index_tree,
        "repo index tree changed: before={} after={}",
        before.index_tree, after.index_tree
    );
}

/// Assert that `cwd` is inside (or equal to) `worktree_root`, resolving
/// symlinks along the way.
///
/// # Panics
/// Panics if `cwd` is not contained within `worktree_root`.
pub fn assert_cwd_inside_worktree(cwd: &str, worktree_root: &str) {
    let resolved_cwd = resolve_path(cwd);
    let resolved_root = resolve_path(worktree_root);

    if resolved_cwd != resolved_root
        && !resolved_cwd.starts_with(&format!("{}/", resolved_root))
    {
        panic!(
            "cwd '{}' (resolved: '{}') is not inside worktree root '{}' (resolved: '{}')",
            cwd, resolved_cwd, worktree_root, resolved_root
        );
    }
}

// -----------------------------------------------------------------
// Internal helpers
// -----------------------------------------------------------------

/// Resolve a path as best-effort: try `canonicalize`, then fall back to
/// resolving from the deepest existing parent.
fn resolve_path(path: &str) -> String {
    let p = Path::new(path);

    // Try full canonicalization first.
    if let Ok(canon) = p.canonicalize() {
        if !canon.as_os_str().is_empty() {
            return canon.to_string_lossy().to_string();
        }
    }

    // If the path is absolute but doesn't exist, walk up to find the
    // deepest existing parent and canonicalize from there.
    if p.is_absolute() {
        if let Some(resolved) = resolve_from_existing_parent(p) {
            return resolved;
        }
    }

    // Last resort: absolute via std::fs::canonicalize on what exists,
    // or return the original string.
    if let Ok(abs) = p.canonicalize() {
        return abs.to_string_lossy().to_string();
    }

    path.to_string()
}

/// Walk up from `path` until we find an existing directory, canonicalize
/// that, then re-attach the missing tail components.
fn resolve_from_existing_parent(path: &Path) -> Option<String> {
    let mut missing: Vec<String> = Vec::new();
    let mut current = path.to_path_buf();

    loop {
        if let Ok(info) = current.metadata() {
            if info.is_dir() {
                // current exists and is a dir
                if let Ok(resolved_parent) = current.canonicalize() {
                    let mut result = resolved_parent;
                    for seg in missing.iter().rev() {
                        result.push(seg);
                    }
                    return Some(result.to_string_lossy().to_string());
                }
                return None;
            }
        }
        // Walk up
        let parent = match current.parent() {
            Some(p) => p.to_path_buf(),
            None => return None,
        };
        if parent == current {
            return None;
        }
        missing.push(
            current
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
        );
        current = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cwd_evidence_roundtrip() {
        let evidence = CWDEvidence {
            cwd: "/tmp/test".into(),
            args: vec!["--mode".into(), "cwd-evidence".into()],
            env: HashMap::from([("HOME".into(), "/tmp".into())]),
            timestamp: "2025-01-01T00:00:00Z".into(),
            mode: "cwd-evidence".into(),
            pid: 12345,
        };
        let json = serde_json::to_string(&evidence).unwrap();
        let decoded: CWDEvidence = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.cwd, "/tmp/test");
        assert_eq!(decoded.pid, 12345);
    }

    #[test]
    fn test_assert_repo_unchanged_pass() {
        let snap = RepoSnapshot {
            head: "abc123".into(),
            status_porcelain: String::new(),
            index_tree: "def456".into(),
            current_branch: "main".into(),
            worktree_list_text: String::new(),
        };
        assert_repo_unchanged(&snap, &snap); // should not panic
    }

    #[test]
    #[should_panic(expected = "repo head changed")]
    fn test_assert_repo_unchanged_fail() {
        let before = RepoSnapshot {
            head: "abc".into(),
            status_porcelain: String::new(),
            index_tree: "def".into(),
            current_branch: "main".into(),
            worktree_list_text: String::new(),
        };
        let after = RepoSnapshot {
            head: "xyz".into(),
            status_porcelain: String::new(),
            index_tree: "def".into(),
            current_branch: "main".into(),
            worktree_list_text: String::new(),
        };
        assert_repo_unchanged(&before, &after);
    }

    #[test]
    fn test_assert_cwd_inside_worktree_exact_match() {
        // If both resolve to the same path, it passes.
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert_cwd_inside_worktree(&cwd, &cwd);
    }
}
