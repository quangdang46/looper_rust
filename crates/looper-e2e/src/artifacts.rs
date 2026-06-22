use std::path::PathBuf;

/// Environment variable that, when set, enables persistent artifact collection.
pub const ENV_E2E_ARTIFACT_ROOT: &str = "LOOPER_E2E_ARTIFACT_ROOT";

/// Return the base directory for this test's artifacts, if the
/// `LOOPER_E2E_ARTIFACT_ROOT` env-var is set.
///
/// The test name is sanitised to contain only `[A-Za-z0-9._-]` characters
/// and appended as a sub-directory.
pub fn artifact_base_dir(test_name: &str) -> Option<PathBuf> {
    let root = std::env::var(ENV_E2E_ARTIFACT_ROOT).ok()?;
    if root.is_empty() {
        return None;
    }
    let sanitised: String = test_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '-' })
        .collect();
    let dir_name = if sanitised.is_empty() { "test".to_string() } else { sanitised };
    Some(PathBuf::from(root).join(dir_name))
}

/// Create a temporary directory, optionally under the artifact root.
///
/// If `LOOPER_E2E_ARTIFACT_ROOT` is set, the directory is created under
/// `<root>/<sanitised-test-name>/<prefix>-<random>/` so artifacts survive
/// the test run.  Otherwise falls back to `std::env::temp_dir()`.
///
/// The returned directory persists until the caller removes it (no automatic
/// cleanup, matching the Go `os.MkdirTemp` behaviour).
///
/// # Panics
/// Panics if `mkdir` fails.
pub fn artifact_temp_dir(_test_name: &str, prefix: &str) -> PathBuf {
    if let Some(named_root) = artifact_base_dir(_test_name) {
        // If we have a named artifact root, use it so artifacts survive.
        std::fs::create_dir_all(&named_root).ok();
        let dir = tempfile::Builder::new()
            .prefix(&format!("{}-", prefix))
            .tempdir_in(&named_root)
            .expect("failed to create artifact temp dir in root")
            .keep();
        return dir;
    }

    // Fallback: standard system temp dir.
    let dir = tempfile::Builder::new()
        .prefix(&format!("{}-", prefix))
        .tempdir()
        .expect("failed to create artifact temp dir")
        .keep();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_artifact_base_dir_no_env() {
        // Without the env var, this should return None
        // We use a scope guard so we don't clobber other tests.
        std::env::remove_var(ENV_E2E_ARTIFACT_ROOT);
        assert!(artifact_base_dir("my_test").is_none());
    }

    #[test]
    fn test_artifact_temp_dir_creates_path() {
        let dir = artifact_temp_dir("test", "prefix");
        // The TempDir is deleted on drop, so we only check the path is valid.
        let name = dir.file_name().unwrap().to_string_lossy();
        assert!(name.contains("prefix"), "expected prefix in dir name, got {}", name);
        // Clean up any leftover.
        let _ = std::fs::remove_dir_all(&dir);
    }
}
