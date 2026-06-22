use std::path::Path;

/// Check config file permissions and log a warning if the file is
/// world-readable.
///
/// On Unix, this checks that the config file's Unix permissions are not
/// world-readable (mode 0600 or stricter). On non-Unix platforms this is a
/// no-op.
///
/// # Returns
///
/// * `Ok(warning)` — `Some(msg)` if permissions are weak, `None` if secure.
/// * `Err(detail)` — If the file could not be stat'd (permissions cannot be
///   determined).
///
/// # Examples
///
/// ```rust,ignore
/// let result = ensure_config_file_permissions(Path::new("/etc/looper/config.toml"));
/// ```
pub fn ensure_config_file_permissions(path: &Path) -> Result<Option<String>, String> {
    if !path.exists() {
        return Ok(None);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = path.metadata().map_err(|e| {
            format!("cannot stat config file {}: {e}", path.display())
        })?;
        let mode = metadata.permissions().mode();

        // Check for world-readable bits (S_IROTH = 0o004, S_IWOTH = 0o002)
        const S_IROTH: u32 = 0o004;
        const S_IWOTH: u32 = 0o002;

        if mode & (S_IROTH | S_IWOTH) != 0 {
            return Ok(Some(format!(
                "config file {} has permissions {:#o}; \
                 consider restricting to 0600 to avoid leaking secrets",
                path.display(),
                mode & 0o777
            )));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    /// Create a temporary file with the given name, write content, and return
    /// the path together with a `TempDir` guard that keeps the file alive.
    fn tmp_file(name: &str, content: &[u8], mode: u32) -> (PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        fs::write(&path, content).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        (path, dir) // keep dir alive
    }

    #[test]
    fn test_secure_permissions_no_warning() {
        let (path, _dir) = tmp_file("secure_config.toml", b"key = \"value\"", 0o600);

        let result = ensure_config_file_permissions(&path).unwrap();
        assert!(result.is_none(), "0600 should be secure");
    }

    #[test]
    fn test_world_readable_triggers_warning() {
        let (path, _dir) = tmp_file("world_readable.toml", b"secret = \"s3kr3t\"", 0o644);

        let result = ensure_config_file_permissions(&path).unwrap();
        assert!(
            result.is_some(),
            "0644 should trigger a permissions warning"
        );
        let warning = result.unwrap();
        assert!(warning.contains("world"));
        assert!(warning.contains("0600"));
    }

    #[test]
    fn test_missing_file_no_error() {
        let result = ensure_config_file_permissions(Path::new("/nonexistent/looper.toml"));
        assert!(result.is_ok(), "missing file should not error");
        assert!(result.unwrap().is_none());
    }
}
