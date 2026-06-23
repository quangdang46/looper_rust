//! Daemon bootstrap sequence: tool validation, directory setup, logger creation.

use std::path::{Path, PathBuf};

use looper_config::Config;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

use crate::error::{BootError, DirError};
use crate::shell;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Result of a successful bootstrap sequence.
pub struct BootstrapOutput {
    pub logger_guard: Option<WorkerGuard>,
}

/// Validate that required CLI tools are available on PATH.
#[allow(clippy::disallowed_methods)]
pub fn validate_tools(_config: &Config) -> Result<(), BootError> {
    for tool in &["git", "gh"] {
        let result = shell::run_command("which", &[tool], ".");
        match result {
            Ok(r) if r.exit_code == 0 => {}
            _ => {
                return Err(BootError::ToolNotFound {
                    tool: tool.to_string(),
                    path: PathBuf::from(tool),
                });
            }
        }
    }
    Ok(())
}

/// Ensure runtime directories exist and are writable.
pub fn ensure_dirs(config: &Config) -> Result<(), BootError> {
    // 1. Log directory — create if missing, verify writable
    if let Some(ref log_path) = config.logging.as_ref().and_then(|l| l.file_path.as_ref()) {
        if let Some(parent) = Path::new(log_path).parent() {
            ensure_writable_dir(parent)?;
        }
    }

    // 2. DB parent directory — create if missing
    if let Some(ref storage) = config.storage {
        if let Some(ref db_path) = storage.path {
            if let Some(parent) = Path::new(db_path).parent() {
                ensure_writable_dir(parent)?;
            }
        }
    }

    Ok(())
}

/// Create a tracing subscriber writing JSON to both a rolling file and stdout/stderr.
///
/// Returns a `WorkerGuard` that must be kept alive for the duration of the process.
pub fn create_logger(config: &Config) -> Result<Option<WorkerGuard>, BootError> {
    let log_level = config
        .logging
        .as_ref()
        .map(|l| l.level.to_string())
        .unwrap_or_else(|| "info".to_string());

    let env_filter = EnvFilter::try_new(&log_level)
        .or_else(|_| EnvFilter::try_new("info"))
        .map_err(|e| BootError::Logger(format!("invalid log level: {e}")))?;

    // Build layers
    let mut layers: Vec<Box<dyn Layer<_> + Send + Sync>> = Vec::new();

    // File layer (if file_path configured)
    let guard = if let Some(ref log_path_str) = config.logging.as_ref().and_then(|l| l.file_path.as_ref()) {
        let log_dir = Path::new(log_path_str).parent().unwrap_or(Path::new("/tmp"));

        // Rolling file appender: rotate daily, keep up to 7 files
        let rolling = tracing_appender::rolling::daily(log_dir, "looperd.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(rolling);

        let file_layer = fmt::layer()
            .json()
            .with_writer(non_blocking)
            .with_target(true)
            .with_thread_ids(false)
            .with_thread_names(false)
            .with_file(false)
            .with_line_number(false)
            .with_ansi(false)
            .boxed();

        layers.push(file_layer);
        Some(guard)
    } else {
        None
    };

    // stdout/stderr layer
    {
        let stdout_layer = fmt::layer()
            .json()
            .with_writer(std::io::stdout)
            .with_target(true)
            .with_ansi(false)
            .boxed();
        layers.push(stdout_layer);
    }

    // Combine layers
    tracing_subscriber::registry()
        .with(env_filter)
        .with(layers)
        .try_init()
        .map_err(|e| BootError::Logger(format!("failed to init logger: {e}")))?;

    Ok(guard)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn validate_executable(name: &str, path: &Path) -> Result<(), BootError> {
    if !path.exists() {
        return Err(BootError::ToolNotFound {
            tool: name.to_string(),
            path: path.to_path_buf(),
        });
    }
    if !path.is_file() {
        return Err(BootError::ToolNotExecutable {
            tool: name.to_string(),
            path: path.to_path_buf(),
        });
    }
    // On Unix, check executable permission
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path).map_err(BootError::Io)?;
        let mode = meta.permissions().mode();
        if mode & 0o111 == 0 {
            return Err(BootError::ToolNotExecutable {
                tool: name.to_string(),
                path: path.to_path_buf(),
            });
        }
    }
    Ok(())
}

fn ensure_writable_dir(path: &Path) -> Result<(), BootError> {
    if path.exists() {
        if !path.is_dir() {
            return Err(BootError::DirError(DirError::IsFile {
                path: path.to_path_buf(),
            }));
        }
    } else {
        std::fs::create_dir_all(path).map_err(|e| {
            BootError::DirError(DirError::Create {
                path: path.to_path_buf(),
                source: e,
            })
        })?;
    }

    // Verify writable with temp file
    let probe = tempfile::Builder::new()
        .prefix(".looper_writable_probe")
        .tempfile_in(path)
        .map_err(|_| {
            BootError::DirError(DirError::NotWritable {
                path: path.to_path_buf(),
            })
        })?;
    let _ = probe; // auto-clean on drop

    Ok(())
}
