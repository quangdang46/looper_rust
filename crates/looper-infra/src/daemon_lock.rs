use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::BootError;

// ---------------------------------------------------------------------------
// DaemonLock — single-instance enforcement via PID file + flock
// ---------------------------------------------------------------------------

/// A file-based lock that prevents multiple daemon instances.
///
/// Acquires an exclusive `flock` on the PID file at construction and releases
/// it on `Drop`. Writes the current PID into the file for diagnostic purposes.
///
/// # Errors
///
/// Returns [`BootError::AlreadyRunning`] if the lock cannot be acquired
/// (another instance is running).
///
/// # Examples
///
/// ```rust,no_run
/// use looper_infra::daemon_lock::DaemonLock;
/// use std::path::Path;
///
/// let lock = DaemonLock::new(Path::new("/tmp/looper.pid"));
/// match lock {
///     Ok(_) => println!("Lock acquired (single instance)"),
///     Err(e) => eprintln!("Failed: {e}"),
/// }
/// // lock released on drop
/// ```
#[derive(Debug)]
pub struct DaemonLock {
    path: PathBuf,
    #[allow(dead_code)]
    file: File,
}

impl DaemonLock {
    /// Acquire the daemon lock at `path`.
    ///
    /// Creates the file if it does not exist, writes the current PID, and
    /// acquires an exclusive `flock(2)`. Returns
    /// [`BootError::AlreadyRunning`] if the lock is held by another process.
    pub fn new(path: &Path) -> Result<Self, BootError> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| BootError::SetupFailed {
                detail: format!("cannot create lock directory {}: {e}", parent.display()),
            })?;
        }

        // Open (or create) the PID file
        let file = File::options()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| BootError::SetupFailed {
                detail: format!("cannot open PID file {}: {e}", path.display()),
            })?;

        // Acquire exclusive flock
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let fd = file.as_raw_fd();
            let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if ret != 0 {
                let err = io::Error::last_os_error();
                return if err.kind() == io::ErrorKind::WouldBlock {
                    Err(BootError::AlreadyRunning {
                        pid_file: path.display().to_string(),
                    })
                } else {
                    Err(BootError::SetupFailed {
                        detail: format!("flock error on {}: {err}", path.display()),
                    })
                };
            }
        }

        #[cfg(not(unix))]
        {
            let _ = &file; // suppress unused
            // On non-Unix platforms, we skip the flock and just create the file.
            // This is a best-effort lock; the binary is primarily deployed on Unix.
        }

        // Write PID
        let pid = std::process::id();
        // Truncate first, then write
        file.set_len(0).ok();
        writeln!(&file, "{pid}").map_err(|e| BootError::SetupFailed {
            detail: format!("cannot write PID to {}: {e}", path.display()),
        })?;

        // Flush to ensure the content is on disk
        file.sync_all().ok();

        Ok(Self {
            path: path.to_path_buf(),
            file,
        })
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        // The file handle is closed automatically, releasing the flock.
        // Attempt to remove the PID file for cleanliness.
        fs::remove_file(&self.path).ok();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_acquire_and_release_lock() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("daemon.pid");

        // First lock should succeed
        let lock1 = DaemonLock::new(&pid_path);
        assert!(lock1.is_ok(), "first lock acquisition should succeed");

        // File should contain the PID
        let content = fs::read_to_string(&pid_path).unwrap();
        let pid: u32 = content.trim().parse().unwrap();
        assert_eq!(pid, std::process::id());

        // Drop the lock (releases flock + deletes file)
        drop(lock1);
        assert!(!pid_path.exists(), "PID file should be removed on drop");

        // Should be able to re-acquire after release
        let lock2 = DaemonLock::new(&pid_path);
        assert!(lock2.is_ok(), "re-acquisition after release should succeed");
        drop(lock2);
    }

    #[test]
    fn test_second_acquire_fails() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("daemon2.pid");

        let _lock1 = DaemonLock::new(&pid_path).unwrap();

        // Second lock on the same path should fail with AlreadyRunning
        let lock2 = DaemonLock::new(&pid_path);
        assert!(
            lock2.is_err(),
            "concurrent lock acquisition should fail"
        );
        match lock2.unwrap_err() {
            BootError::AlreadyRunning { .. } => {} // expected
            other => panic!("expected AlreadyRunning, got: {other:?}"),
        }
    }
}
