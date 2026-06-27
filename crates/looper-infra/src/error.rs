use std::path::PathBuf;
use thiserror::Error;

// ---------------------------------------------------------------------------
// BootError — wraps all failures during daemon startup
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum BootError {
    #[error("config error: {0}")]
    Config(#[from] looper_config::ConfigError),

    #[error("tool not found: {tool} — expected at {path:?}")]
    ToolNotFound { tool: String, path: PathBuf },

    #[error("tool not executable: {tool} at {path:?}")]
    ToolNotExecutable { tool: String, path: PathBuf },

    #[error("directory error: {0}")]
    DirError(#[from] DirError),

    #[error("logger setup failed: {0}")]
    Logger(String),

    #[error("setup failed: {detail}")]
    SetupFailed { detail: String },

    #[error("daemon already running — PID file exists: {pid_file}")]
    AlreadyRunning { pid_file: String },

    #[error("setup error: {0}")]
    Setup(#[from] SetupError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// DirError — directory creation / writability checks
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum DirError {
    #[error("cannot create directory {path:?}: {source}")]
    Create { path: PathBuf, source: std::io::Error },

    #[error("directory {path:?} is not writable")]
    NotWritable { path: PathBuf },

    #[error("path {path:?} is a file, not a directory")]
    IsFile { path: PathBuf },

    #[error("I/O error during directory check: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// SetupError — failures inside Setup (dependency graph assembly)
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum SetupError {
    #[error("coordinator open failed: {0}")]
    Coordinator(String),

    #[error("migration failed: {0}")]
    Migration(String),

    #[error("dependency build failed: {0}")]
    DependencyBuild(String),

    #[error("project sync failed: {0}")]
    ProjectSync(String),

    #[error("invalid config: {0}")]
    Config(String),

    #[error("daemon lock acquisition failed: {0}")]
    DaemonLock(String),

    #[error("scheduler build failed: {0}")]
    Scheduler(String),

    #[error("recovery pipeline failed: {0}")]
    Recovery(String),

    #[error("network manager failed: {0}")]
    Network(String),

    #[error("webhook subsystem failed: {0}")]
    Webhook(String),

    #[error("worktree cleanup setup failed: {0}")]
    WorktreeCleanup(String),
}

// ---------------------------------------------------------------------------
// RuntimeError — runtime lifecycle errors
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error("runtime already started")]
    AlreadyStarted,

    #[error("runtime already stopped")]
    AlreadyStopped,

    #[error("runtime not started")]
    NotStarted,

    #[error("shutdown timeout for {component} after {timeout_ms}ms")]
    ShutdownTimeout { component: &'static str, timeout_ms: u64 },

    #[error("startup phase failed: {0}")]
    Startup(String),

    #[error("shutdown phase failed: {0}")]
    Shutdown(String),

    #[error("setup error: {0}")]
    Setup(#[from] SetupError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// NotifyError — notification subsystem
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum NotifyError {
    #[error("event log append failed: {0}")]
    EventLog(String),

    #[error("osascript execution failed: {0}")]
    Osascript(String),

    #[error("osascript not available: {0}")]
    OsascriptNotAvailable(String),

    #[error("throttle failure: {0}")]
    Throttle(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// CleanupError — worktree cleanup errors
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum CleanupError {
    #[error("plan failed: {0}")]
    Plan(String),

    #[error("execute failed: {0}")]
    Execute(String),

    #[error("worktree path {path:?} is unsafe: {reason}")]
    UnsafePath { path: PathBuf, reason: String },

    #[error("git operation failed: {0}")]
    Git(String),

    #[error("database error: {0}")]
    Database(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
