#![allow(clippy::arc_with_non_send_sync)]
//! Looper daemon binary — main entry point.
//!
//! Bootstrap sequence (from spec §6.1):
//! 1. Load config from CLI args + env + defaults
//! 2. Validate tool paths (git, gh)
//! 3. Ensure runtime directories
//! 4. Create logger
//! 5. Open SQLite, run migrations
//! 6. Initialize scheduler + runtime
//! 7. Start API server
//! 8. Wait for shutdown signal

mod version;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use clap::Parser;
use uuid::Uuid;
use looper_agent::executor::ConfiguredExecutor;
use looper_agent::types::{AgentCliVendor, ExecutorConfig};
use looper_api::{
    AddProjectInput, ApiError, Context, ProjectService as ProjectServiceTrait, ProjectSummary,
    RuntimeState, ServerConfig,
};
use looper_config::Config;
use looper_github::gateway::{Gateway, GatewayOptions};
use looper_git::Gateway as GitGateway;
use looper_infra::{bootstrap, Runtime as DaemonRuntime};
use looper_runner::{Coordinator, Fixer, Planner, Reviewer, Worker};
use looper_scheduler::scheduler::SendRepos;
use looper_scheduler::active_executions::ActiveExecutionRegistry;
use looper_scheduler::types::{HandlerMap, SchedulerConfig, ThreadRunner};
use looper_scheduler::Scheduler;
use looper_service::ProjectService;
use looper_storage::{
    record::ProjectRecord,
    repos::events::EventsRepository, run_migrations, EventLog, Repositories,
};
use rusqlite::Connection;
use tokio::sync::oneshot;

// ---------------------------------------------------------------------------
// CLI Args
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "looperd", version = version::value(), about = "Looper daemon")]
struct Args {
    /// Path to config file
    #[arg(short = 'c', long)]
    config: Option<String>,

    /// Print version info as JSON and exit
    #[arg(long)]
    json: bool,
}

// ---------------------------------------------------------------------------
// DaemonState — implements RuntimeState
// ---------------------------------------------------------------------------

struct DaemonState {
    repos: Arc<Repositories>,
    event_log: EventLog,
    scheduler: Option<Arc<Scheduler>>,
}

// rusqlite::Connection uses RefCell internally (!Sync), making Arc<Repositories>
// and EventLog !Send. Under a single tokio::main all access is serialized.
unsafe impl Send for DaemonState {}
unsafe impl Sync for DaemonState {}

fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

#[async_trait]
impl RuntimeState for DaemonState {
    fn repos(&self) -> &Repositories {
        &self.repos
    }

    fn repos_arc(&self) -> Arc<Repositories> {
        self.repos.clone()
    }

    fn event_log(&self) -> &EventLog {
        &self.event_log
    }

    async fn stop_loop(&self, _project_name: &str, loop_seq: i64) -> Result<(), ApiError> {
        let mut rec = self
            .repos
            .loops
            .get_by_seq(loop_seq)
            .map_err(|e| ApiError::internal(format!("get loop: {e}")))?
            .ok_or_else(|| ApiError::not_found(format!("loop seq={loop_seq}")))?;
        let finished = now_iso();
        rec.status = "stopped".into();
        rec.updated_at = finished.clone();
        self.repos
            .loops
            .upsert(&rec)
            .map_err(|e| ApiError::internal(format!("upsert loop: {e}")))?;
        
        self.repos
            .queue
            .cancel_by_loop(&rec.id, &finished, Some("stopped by user"))
            .map_err(|e| ApiError::internal(format!("cancel queue: {e}")))?;
        Ok(())
    }

    async fn close_loop(&self, _project_name: &str, loop_seq: i64) -> Result<(), ApiError> {
        let mut rec = self
            .repos
            .loops
            .get_by_seq(loop_seq)
            .map_err(|e| ApiError::internal(format!("get loop: {e}")))?
            .ok_or_else(|| ApiError::not_found(format!("loop seq={loop_seq}")))?;
        let finished = now_iso();
        rec.status = "closed".into();
        rec.updated_at = finished.clone();
        self.repos
            .loops
            .upsert(&rec)
            .map_err(|e| ApiError::internal(format!("upsert loop: {e}")))?;
        self.repos
            .queue
            .cancel_by_loop(&rec.id, &finished, Some("loop closed"))
            .map_err(|e| ApiError::internal(format!("cancel queue: {e}")))?;
        Ok(())
    }

    async fn stop_all(&self, project_name: &str) -> Result<(), ApiError> {
        let finished = now_iso();
        self.repos
            .queue
            .cancel_by_project(project_name, &finished, Some("all stopped by user"))
            .map_err(|e| ApiError::internal(format!("cancel queue: {e}")))?;
        Ok(())
    }

    async fn repair_reviewer(&self, project_name: &str) -> Result<(), ApiError> {
        // Find the project by name
        let projects = self
            .repos
            .projects
            .list()
            .map_err(|e| ApiError::internal(format!("list projects: {e}")))?;
        let project = projects
            .iter()
            .find(|p| p.name == project_name)
            .ok_or_else(|| ApiError::not_found(format!("project '{project_name}'")))?
            .clone();

        let now = now_iso();
        let dedupe_key = format!("repair_reviewer:{}", project.id);
        let record = looper_storage::record::QueueItemRecord {
            id: Uuid::new_v4().to_string(),
            project_id: Some(project.id),
            loop_id: None,
            r#type: "reviewer".to_string(),
            target_type: "repair".to_string(),
            target_id: project_name.to_string(),
            repo: None,
            pr_number: None,
            dedupe_key,
            priority: 50,
            status: "queued".to_string(),
            available_at: now.clone(),
            attempts: 0,
            max_attempts: 3,
            claimed_by: None,
            claimed_at: None,
            started_at: None,
            finished_at: None,
            lock_key: None,
            payload_json: None,
            last_error: None,
            last_error_kind: None,
            created_at: now.clone(),
            updated_at: now,
        };
        self.repos
            .queue
            .create_or_get_active_by_dedupe(&record)
            .map_err(|e| ApiError::internal(format!("enqueue repair_reviewer: {e}")))?;
        tracing::info!(project_name, "repair_reviewer enqueued");

        // Trigger scheduler so it picks up the new item
        if let Some(ref sched) = self.scheduler {
            sched.trigger_tick();
        }
        Ok(())
    }

    async fn trigger_scheduler_tick(&self) {
        if let Some(ref sched) = self.scheduler {
            sched.trigger_tick();
        }
    }
}

// ---------------------------------------------------------------------------
// DaemonProjectService — implements ProjectService (async trait)
// ---------------------------------------------------------------------------

struct DaemonProjectService {
    service: looper_service::ProjectService,
}

unsafe impl Send for DaemonProjectService {}
unsafe impl Sync for DaemonProjectService {}

#[async_trait]
impl ProjectServiceTrait for DaemonProjectService {
    async fn add(&self, input: AddProjectInput) -> Result<ProjectSummary, ApiError> {
        let service_input = looper_service::AddInput {
            id: input.name.clone(),
            name: input.name,
            // Map API `path` -> local repo path used for detection,
            // and `repo_url` -> GitHub repo URL.
            repo_path: input.path.unwrap_or_default(),
            base_branch: input.default_branch.unwrap_or_else(|| "main".into()),
            id_source: "explicit".into(),
            worktree_root: None,
            repo: input.repo_url,
            snapshot_mode: looper_service::SnapshotMode::Async,
        };
        let result = self
            .service
            .add_project(service_input)
            .map_err(|e| ApiError::internal(e.to_string()))?;
        Ok(ProjectSummary {
            name: result.project.id,
            path: result.project.repo_path.clone(),
            repo_url: result.repo,
            default_branch: result.project.base_branch.unwrap_or_default(),
            schedule: String::new(),
            enabled: !result.project.archived,
            archive_filter: None,
        })
    }

    async fn remove(&self, name: &str) -> Result<(), ApiError> {
        self.service
            .remove_project(name)
            .map_err(|e| ApiError::internal(e.to_string()))?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ProjectSummary>, ApiError> {
        let records = self
            .service
            .list()
            .map_err(|e| ApiError::internal(e.to_string()))?;
        Ok(records
            .into_iter()
            .map(|r| project_record_to_summary(r))
            .collect())
    }

    async fn sync(&self, name: &str) -> Result<ProjectSummary, ApiError> {
        let records = self
            .service
            .list()
            .map_err(|e| ApiError::internal(e.to_string()))?;
        let rec = records
            .into_iter()
            .find(|r| r.id == name)
            .ok_or_else(|| ApiError::not_found(name))?;
        Ok(project_record_to_summary(rec))
    }
}

/// Convert a ProjectRecord into the API's ProjectSummary, extracting the
/// repo_url from the metadata_json payload (stored as `{"repo": ...}`).
fn project_record_to_summary(rec: ProjectRecord) -> ProjectSummary {
    let repo_url = rec
        .metadata_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("repo").and_then(|r| r.as_str().map(String::from)));
    ProjectSummary {
        name: rec.id,
        path: rec.repo_path.clone(),
        repo_url,
        default_branch: rec.base_branch.unwrap_or_default(),
        schedule: String::new(),
        enabled: !rec.archived,
        archive_filter: None,
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("FATAL: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // --version JSON
    if args.json {
        let info = version::VersionInfo::current();
        println!("{}", serde_json::to_string_pretty(&info)?);
        return Ok(());
    }

    // 1. Load config
    let config = {
        let loader = looper_config::ConfigLoader::new();
        let loader = match &args.config {
            Some(path) => loader.with_config_path(std::path::PathBuf::from(path)),
            None => loader,
        };
        loader.load()
    }.map_err(|e| format!("config load: {e}"))?;

    // 2. Validate tool paths
    bootstrap::validate_tools(&config).map_err(|e| format!("tool validation: {e}"))?;

    // 3. Ensure runtime directories
    bootstrap::ensure_dirs(&config).map_err(|e| format!("dir setup: {e}"))?;

    // 4. Create logger
    let _guard = bootstrap::create_logger(&config).map_err(|e| format!("logger: {e}"))?;
    tracing::info!("looperd starting (version {})", version::value());

    // 5. Open SQLite, run migrations
    let db_path = resolve_db_path(&config);

    // Primary connection for API handlers (via DaemonState)
    let conn = open_db(&db_path)?;
    let repos = Arc::new(Repositories::new(conn));

    // Event-log connection (separate to avoid contention)
    let event_log_conn = open_db(&db_path)?;
    let event_log = EventLog::new(EventsRepository::new(Arc::new(event_log_conn)));

    // Scheduler connection (separate — wrapped in Mutex for thread-safety)
    let sched_conn = open_db(&db_path)?;
    let send_repos = Arc::new(SendRepos(std::sync::Mutex::new(Repositories::new(
        sched_conn,
    ))));

    // 6. Build HandlerMap with all 5 runners + GitHub/git/agent gateways
    let scheduler_cfg = SchedulerConfig::default();
    let github: Option<Arc<Gateway>> = Some(Arc::new(Gateway::new(GatewayOptions::default())));

    // Agent executor gateway — uses its own connection to avoid
    // RefCell contention with the API handler's connection.
    let agent_cfg = ExecutorConfig {
        vendor: AgentCliVendor::ClaudeCode,
        model: None,
        params: std::collections::HashMap::new(),
        env: std::collections::HashMap::new(),
        native_resume_enabled: true,
    };
    let log_dir = config
        .defaults
        .as_ref()
        .and_then(|d| d.log_dir.as_deref())
        .unwrap_or("/tmp/looper/logs")
        .to_string();

    // Open a dedicated connection for agent DB operations so
    // RefCell contention with the API connection is avoided.
    // Also wrap in Mutex for concurrent agent calls (write-spec runs
    // on parallel pipeline threads).
    let agent_conn = open_db(&db_path)?;
    let agent_repos = Arc::new(std::sync::Mutex::new(Repositories::new(agent_conn)));
    let agent: Option<Arc<ConfiguredExecutor>> = Some(Arc::new(ConfiguredExecutor::new(
        agent_cfg,
        agent_repos,
        log_dir,
        Utc::now,
    )));

    // Git gateway
    let git: Option<Arc<GitGateway>> = Some(Arc::new(GitGateway::new(
        looper_git::types::GatewayOptions {
            git_path: "git".to_string(),
            repos: None,
            now: Utc::now,
        },
    )));

    let tokio_handle = tokio::runtime::Handle::current();
    let handlers = HandlerMap {
        planner: Some(Arc::new(Planner::new(
            &scheduler_cfg,
            Arc::clone(&send_repos),
            github.clone(),
            agent.clone(),
            git.clone(),
            tokio_handle.clone(),
        ))),
        coordinator: Some(Arc::new(Coordinator::new(
            &scheduler_cfg,
            Arc::clone(&send_repos),
            github.clone(),
            tokio_handle.clone(),
        ))),
        reviewer: Some(Arc::new(Reviewer::new(
            &scheduler_cfg,
            Arc::clone(&send_repos),
            github.clone(),
            tokio_handle.clone(),
            agent.clone(),
            git.clone(),
        ))),
        fixer: Some(Arc::new(Fixer::new(
            &scheduler_cfg,
            Arc::clone(&send_repos),
            github.clone(),
            tokio_handle.clone(),
            agent.clone(),
            git.clone(),
        ))),
        worker: Some(Arc::new(Worker::new(
            &scheduler_cfg,
            Arc::clone(&send_repos),
            github.clone(),
            tokio_handle.clone(),
            agent.clone(),
            git.clone(),
        ))),
        snapshot: None,
    };

    // 7. Build Scheduler
    let scheduler = Arc::new(Scheduler::new(
        scheduler_cfg,
        handlers,
        Box::new(ThreadRunner),
        None,
        Arc::new(ActiveExecutionRegistry::new()),
        send_repos,
        Utc::now,
    ));

    // 8. Create Runtime (stores scheduler, starts it in complete_startup)
    let runtime = DaemonRuntime::new(config.clone());
    runtime.start(
        Arc::clone(&repos),
        event_log,
        Some(Arc::clone(&scheduler)),
    )?;

    // 9. Build API context
    let state = Arc::new(DaemonState {
        repos: Arc::clone(&repos),
        event_log: EventLog::new(EventsRepository::new(Arc::new(open_db(&db_path)?))),
        scheduler: Some(Arc::clone(&scheduler)),
    });
    let project_svc = ProjectService::new(
        Arc::clone(&repos),
        looper_service::ProjectServiceCallbacks::new(),
        Utc::now,
    );
    let projects = Arc::new(DaemonProjectService {
        service: project_svc,
    });
    let ctx = Arc::new(Context {
        config: Arc::new(config.clone()),
        state,
        projects,
    });

    // 10. Determine bind address
    let host = config
        .server
        .as_ref()
        .map(|s| s.host.as_str())
        .unwrap_or("127.0.0.1");
    let port = config
        .server
        .as_ref()
        .map(|s| s.port)
        .unwrap_or(7391);
    let api_config = ServerConfig {
        bind_address: format!("{host}:{port}"),
        auth_token: None,
    };

    // 11. Start API server
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let api_handle =
        tokio::spawn(async move { looper_api::serve(ctx, api_config, shutdown_rx).await });

    // 12. Complete startup (starts scheduler threads, worktree cleanup)
    runtime.complete_startup().await?;
    tracing::info!("looperd started on {host}:{port}");

    // 13. Wait for shutdown signal
    wait_for_shutdown().await;
    tracing::info!("looperd shutting down");

    // 14. Graceful shutdown
    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(5), api_handle).await;
    let _ = runtime.stop("shutdown").await;

    tracing::info!("looperd stopped");
    Ok(())
}

/// Resolve the database file path from config.
fn resolve_db_path(config: &Config) -> String {
    config
        .storage
        .as_ref()
        .and_then(|s| s.path.as_deref())
        .unwrap_or("looperd.db")
        .to_string()
}

/// Open a SQLite connection and run migrations.
fn open_db(path: &str) -> Result<Connection, Box<dyn std::error::Error>> {
    let mut conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    run_migrations(&mut conn)?;
    Ok(conn)
}

/// Wait for SIGINT or SIGTERM. Falls back to SIGINT-only if SIGTERM handler can't be registered.
async fn wait_for_shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut sigterm) => {
            tokio::select! {
                _ = ctrl_c => {},
                _ = sigterm.recv() => {},
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not register SIGTERM handler, falling back to SIGINT only");
            ctrl_c.await.ok();
        }
    }
}
