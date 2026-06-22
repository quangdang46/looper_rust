use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use crate::active_executions::ActiveExecutionRegistry;
use crate::claim::claim_and_run;
use crate::error::{SchedulerError, SchedulerResult};
use crate::tick::execute_scheduler_tick;
use crate::types::{
    AsyncRunner, ClaimPhase, Context, HandlerMap, SchedulerConfig, TickSummary,
};
use crate::types::StaleRunReconcileSummary;
use looper_storage::Repositories;

/// Safety: rusqlite::Connection is !Sync, so Arc<Repositories> is !Send.
/// We wrap repos in Mutex ensuring single-threaded access, making this safe.
pub struct SendRepos(pub std::sync::Mutex<Repositories>);
unsafe impl Send for SendRepos {}
unsafe impl Sync for SendRepos {}

/// The main scheduler — manages tick loop, claim pump, discovery, and recovery.
pub struct Scheduler {
    pub tick_interval: Duration,
    pub wake_ch: mpsc::Sender<()>,
    wake_rx: Mutex<Option<mpsc::Receiver<()>>>,
    pub claim_wake_ch: mpsc::Sender<()>,
    claim_wake_rx: Mutex<Option<mpsc::Receiver<()>>>,
    stopped: Arc<AtomicBool>,
    pub(crate) claim_mu: Arc<Mutex<()>>,
    pub max_concurrent_runs: usize,
    pub planner_discovery_enabled: bool,
    pub coordinator_enabled: bool,
    pub reviewer_discovery_enabled: bool,
    pub fixer_discovery_enabled: bool,
    pub worker_discovery_enabled: bool,
    pub handlers: HandlerMap,
    pub async_runner: Box<dyn AsyncRunner>,
    pub reconcile_stale_runs:
        Option<Arc<dyn Fn(&Context) -> SchedulerResult<StaleRunReconcileSummary> + Send + Sync>>,
    pub active_executions: Arc<ActiveExecutionRegistry>,
    /// Wrapped in SendRepos for thread-safety (rusqlite::Connection is !Sync).
    pub repos: Arc<SendRepos>,
    pub now: fn() -> DateTime<Utc>,
    started: AtomicBool,
}

impl Scheduler {
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: SchedulerConfig,
        handlers: HandlerMap,
        async_runner: Box<dyn AsyncRunner>,
        reconcile_stale_runs: Option<
            Arc<dyn Fn(&Context) -> SchedulerResult<StaleRunReconcileSummary> + Send + Sync>,
        >,
        active_executions: Arc<ActiveExecutionRegistry>,
        repos: Arc<SendRepos>,
        now: fn() -> DateTime<Utc>,
    ) -> Self {
        let (wake_ch, wake_rx) = mpsc::channel();
        let (claim_wake_ch, claim_wake_rx) = mpsc::channel();

        Self {
            tick_interval: cfg.poll_interval,
            wake_ch,
            wake_rx: Mutex::new(Some(wake_rx)),
            claim_wake_ch,
            claim_wake_rx: Mutex::new(Some(claim_wake_rx)),
            stopped: Arc::new(AtomicBool::new(false)),
            claim_mu: Arc::new(Mutex::new(())),
            max_concurrent_runs: cfg.max_concurrent_runs,
            planner_discovery_enabled: true,
            coordinator_enabled: true,
            reviewer_discovery_enabled: true,
            fixer_discovery_enabled: true,
            worker_discovery_enabled: true,
            handlers,
            async_runner,
            reconcile_stale_runs,
            active_executions,
            repos,
            now,
            started: AtomicBool::new(false),
        }
    }

    pub fn start(self: &Arc<Self>) -> SchedulerResult<()> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(SchedulerError::AlreadyRunning);
        }

        let stopped = Arc::clone(&self.stopped);

        if self.handlers.has_claim_handler() {
            let scheduler = Arc::clone(self);
            let stopped = stopped.clone();
            let claim_wake_rx = self
                .claim_wake_rx
                .lock()
                .unwrap()
                .take()
                .expect("claim_wake_rx already taken");
            thread::spawn(move || {
                scheduler.run_claim_loop(stopped, claim_wake_rx);
            });
        }

        let scheduler = Arc::clone(self);
        let wake_rx = self.wake_rx.lock().unwrap().take().expect("wake_rx already taken");
        thread::spawn(move || {
            scheduler.run_main_loop(stopped, wake_rx);
        });

        Ok(())
    }

    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = self.wake_ch.send(());
        let _ = self.claim_wake_ch.send(());
    }

    fn run_main_loop(&self, stopped: Arc<AtomicBool>, wake_rx: mpsc::Receiver<()>) {
        let ctx = Context::new();
        self.execute_scheduler_tick(&ctx);

        let poll_interval = self.tick_interval;
        if poll_interval.is_zero() {
            loop {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                if wake_rx.recv().is_ok() {
                    if stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    let ctx = Context::new();
                    self.execute_scheduler_tick(&ctx);
                }
            }
        } else {
            let mut last_tick = Instant::now();
            loop {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let remaining = poll_interval.saturating_sub(last_tick.elapsed());
                if remaining.is_zero() || remaining.as_millis() == 0 {
                    last_tick = Instant::now();
                    let ctx = Context::new();
                    self.execute_scheduler_tick(&ctx);
                } else if wake_rx.recv_timeout(remaining).is_ok() {
                    if stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    last_tick = Instant::now();
                    let ctx = Context::new();
                    self.execute_scheduler_tick(&ctx);
                }
            }
        }
    }

    fn run_claim_loop(&self, stopped: Arc<AtomicBool>, claim_wake_rx: mpsc::Receiver<()>) {
        let ctx = Context::new();
        self.execute_claim_pass(&ctx, ClaimPhase::ClaimPump);

        loop {
            if stopped.load(Ordering::SeqCst) {
                break;
            }
            if claim_wake_rx.recv_timeout(Duration::from_secs(1)).is_ok() {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let ctx = Context::new();
                self.execute_claim_pass(&ctx, ClaimPhase::ClaimPump);
            }
        }
    }

    pub fn trigger_tick(&self) {
        let _ = self.wake_ch.send(());
        let _ = self.claim_wake_ch.send(());
    }

    pub fn trigger_claim(&self) {
        let _ = self.claim_wake_ch.send(());
    }

    pub fn execute_scheduler_tick(&self, ctx: &Context) -> TickSummary {
        execute_scheduler_tick(self, ctx)
    }

    pub fn execute_claim_pass(&self, ctx: &Context, phase: ClaimPhase) {
        let start = Instant::now();

        let _guard = match self.claim_mu.lock() {
            Ok(g) => g,
            Err(_) => {
                tracing::warn!("claim lock contention, skipping claim pass");
                return;
            }
        };

        let available = self.compute_available_slots(ctx);
        if available == 0 {
            return;
        }

        let items = claim_and_run(self, ctx, available);
        let claimed = items.len();

        if claimed > 0 {
            tracing::info!(
                phase = phase.as_str(),
                available,
                claimed,
                duration_ms = start.elapsed().as_millis() as u64,
                "claim pass completed"
            );
        }
    }

    /// Lock repos and return a guard.
    pub fn repos(&self) -> std::sync::MutexGuard<'_, Repositories> {
        self.repos.0.lock().unwrap()
    }

    pub fn compute_available_slots(&self, _ctx: &Context) -> usize {
        if self.max_concurrent_runs == 0 {
            return 0;
        }
        let repos = self.repos();
        let running_count = match repos.queue.count_by_status("running") {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("failed to count running queue items: {e}");
                return 0;
            }
        };
        let max = self.max_concurrent_runs as i64;
        if running_count >= max {
            0
        } else {
            (max - running_count) as usize
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ThreadRunner;

    fn create_test_repos() -> Arc<SendRepos> {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        let _ = looper_storage::migration::run_migrations(&mut conn);
        Arc::new(SendRepos(Mutex::new(Repositories::new(conn))))
    }

    fn make_scheduler(cfg: SchedulerConfig) -> Arc<Scheduler> {
        let handlers = HandlerMap {
            planner: None, coordinator: None, reviewer: None,
            fixer: None, worker: None, snapshot: None,
        };
        Arc::new(Scheduler::new(
            cfg, handlers, Box::new(ThreadRunner), None,
            Arc::new(ActiveExecutionRegistry::new()),
            create_test_repos(), Utc::now,
        ))
    }

    #[test]
    fn test_new_scheduler() {
        let s = make_scheduler(SchedulerConfig::default());
        assert!(!s.started.load(Ordering::SeqCst));
    }

    #[test]
    fn test_trigger_tick_no_block() {
        let s = make_scheduler(SchedulerConfig::default());
        s.trigger_tick();
        s.trigger_claim();
    }

    #[test]
    fn test_compute_available_slots_zero() {
        let cfg = SchedulerConfig { max_concurrent_runs: 5, ..Default::default() };
        let s = make_scheduler(cfg);
        assert_eq!(s.compute_available_slots(&Context::new()), 5);
    }

    #[test]
    fn test_start_shutdown() {
        let s = make_scheduler(SchedulerConfig::default());
        assert!(s.start().is_ok());
        s.shutdown();
        std::thread::sleep(Duration::from_millis(50));
    }

    #[test]
    fn test_start_twice_fails() {
        let s = make_scheduler(SchedulerConfig::default());
        assert!(s.start().is_ok());
        assert!(s.start().is_err());
        s.shutdown();
    }
}
