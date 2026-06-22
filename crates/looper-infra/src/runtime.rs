//! Runtime lifecycle — central state holder for the running daemon.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use looper_config::Config;
use looper_scheduler::Scheduler;
use looper_storage::eventlog::EventLog;
use looper_storage::repos::Repositories;
use tokio::sync::Notify;

use crate::error::RuntimeError;
use crate::worktree_cleanup;

/// Thread-safe wrapper for `Arc<Repositories>`. `Arc<Repositories>` is !Send
/// because `rusqlite::Connection` uses RefCell (not Sync). The cleanup thread
/// is the sole accessor, making this safe.
struct SendRepos(Arc<Repositories>);

// SAFETY: The spawned cleanup thread is the sole user of this repos handle.
unsafe impl Send for SendRepos {}
unsafe impl Sync for SendRepos {}

/// Holds all live state for the running daemon.
pub struct Runtime {
    pub config: Arc<Config>,
    services: OnceLock<Arc<Services>>,
    scheduler: OnceLock<Arc<Scheduler>>,

    // Lifecycle
    started: AtomicBool,
    stopped: AtomicBool,
    pub started_at: OnceLock<String>,
    shutdown_notify: Arc<Notify>,

    // Background handles
    scheduler_handle: OnceLock<tokio::task::JoinHandle<()>>,
    cleanup_tx: OnceLock<std::sync::mpsc::Sender<()>>,
    cleanup_handle: OnceLock<thread::JoinHandle<()>>,
}

/// Thread-safe snapshot of all service-layer components.
pub struct Services {
    pub repos: Arc<Repositories>,
    pub event_log: EventLog,
}

impl Services {
    pub fn new(repos: Arc<Repositories>, event_log: EventLog) -> Self {
        Self { repos, event_log }
    }
}

impl Runtime {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(config),
            services: OnceLock::new(),
            scheduler: OnceLock::new(),
            started: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            started_at: OnceLock::new(),
            shutdown_notify: Arc::new(Notify::new()),
            scheduler_handle: OnceLock::new(),
            cleanup_tx: OnceLock::new(),
            cleanup_handle: OnceLock::new(),
        }
    }

    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    pub fn services(&self) -> Option<&Arc<Services>> {
        self.services.get()
    }

    pub fn scheduler(&self) -> Option<&Arc<Scheduler>> {
        self.scheduler.get()
    }

    // ── Lifecycle: Start ──────────────────────────────────────────────────

    pub fn start(
        &self,
        repos: Arc<Repositories>,
        event_log: EventLog,
        scheduler: Option<Arc<Scheduler>>,
    ) -> Result<(), RuntimeError> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(RuntimeError::AlreadyStarted);
        }
        #[allow(clippy::arc_with_non_send_sync)]
        let services = Arc::new(Services::new(repos, event_log));
        let _ = self.services.set(services);
        if let Some(sched) = scheduler {
            let _ = self.scheduler.set(sched);
        }
        let _ = self.started_at.set(chrono::Utc::now().to_rfc3339());
        Ok(())
    }

    // ── Lifecycle: CompleteStartup ────────────────────────────────────────

    /// Start the scheduler tick loop (tokio) and worktree cleanup (std thread).
    pub async fn complete_startup(&self) -> Result<(), RuntimeError> {
        if self.is_stopped() {
            return Err(RuntimeError::AlreadyStopped);
        }
        if !self.is_started() {
            return Err(RuntimeError::NotStarted);
        }

        // Start scheduler tick loop (tokio task — async)
        if let Some(sched) = self.scheduler.get() {
            let sched = Arc::clone(sched);
            let notify = Arc::clone(&self.shutdown_notify);

            // Start the scheduler's own threads (tick loop, claim pump)
            if let Err(e) = sched.start() {
                tracing::error!(error = %e, "failed to start scheduler");
                return Err(RuntimeError::Startup(format!("scheduler start: {e}")));
            }

            let handle = tokio::spawn(async move {
                notify.notified().await;
                sched.shutdown();
            });
            let _ = self.scheduler_handle.set(handle);
        }

        // Start worktree cleanup loop (std thread — rusqlite is !Sync)
        if let Some(svc) = self.services.get() {
            let repos = Arc::new(SendRepos(Arc::clone(&svc.repos)));
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            let _ = self.cleanup_tx.set(tx);

            let handle = thread::spawn(move || {
                let options = worktree_cleanup::CleanupOptions::default();
                loop {
                    if rx.recv_timeout(Duration::from_secs(3600)).is_ok() {
                        break;
                    }
                    let arg = &repos.0;
                    if let Ok(plan_result) = worktree_cleanup::plan(arg, &options) {
                        if let Ok(result) = worktree_cleanup::run(arg, &plan_result, &options) {
                            tracing::info!(
                                scanned = result.summary.scanned,
                                candidates = result.summary.candidates,
                                "worktree cleanup pass completed"
                            );
                        }
                    }
                }
            });
            let _ = self.cleanup_handle.set(handle);
        }

        Ok(())
    }

    // ── Lifecycle: Stop ───────────────────────────────────────────────────

    pub async fn stop(&self, reason: &str) -> Result<(), RuntimeError> {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return Err(RuntimeError::AlreadyStopped);
        }
        tracing::info!(reason, "looperd runtime stopping");

        if let Some(tx) = self.cleanup_tx.get() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.scheduler_handle.get() {
            handle.abort();
        }

        self.shutdown_notify.notify_waiters();
        tracing::info!(reason, "looperd runtime stopped");
        Ok(())
    }

    // ── Lifecycle: WaitForShutdown ────────────────────────────────────────

    pub async fn wait_for_shutdown(&self) {
        self.shutdown_notify.notified().await;
    }
}
