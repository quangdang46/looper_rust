pub mod projects;
pub mod loops;
pub mod runs;
pub mod agent_executions;
pub mod pull_request_snapshots;
pub mod events;
pub mod locks;
pub mod queue;
pub mod notifications;
pub mod worktrees;
pub mod webhook_forwarders;
pub mod webhook_tunnel_hooks;

use std::sync::Arc;

use rusqlite::Connection;

use self::agent_executions::AgentExecutionsRepository;
use self::events::EventsRepository;
use self::locks::LocksRepository;
use self::loops::LoopsRepository;
use self::notifications::NotificationsRepository;
use self::projects::ProjectsRepository;
use self::pull_request_snapshots::PullRequestSnapshotsRepository;
use self::queue::QueueRepository;
use self::runs::RunsRepository;
use self::webhook_forwarders::WebhookForwardersRepository;
use self::webhook_tunnel_hooks::WebhookTunnelHooksRepository;
use self::worktrees::WorktreesRepository;

/// Container for all repository instances.
///
/// Each sub-repository owns its own `Arc<Connection>`. When created via
/// `open()`, every sub-repo gets a **unique** connection, avoiding the
/// `RefCell` borrow panics that occur when a single `rusqlite::Connection`
/// (which wraps `RefCell<InnerConnection>`) is shared across repos.
///
/// The `new()` constructor exists for tests that pass in a single in-memory
/// connection. Consumers in production should always use `open()`.
pub struct Repositories {
    pub projects: ProjectsRepository,
    pub loops: LoopsRepository,
    pub runs: RunsRepository,
    pub agent_executions: AgentExecutionsRepository,
    pub pull_request_snapshots: PullRequestSnapshotsRepository,
    pub events: EventsRepository,
    pub locks: LocksRepository,
    pub queue: QueueRepository,
    pub notifications: NotificationsRepository,
    pub worktrees: WorktreesRepository,
    pub webhook_forwarders: WebhookForwardersRepository,
    pub webhook_tunnel_hooks: WebhookTunnelHooksRepository,
}

impl Repositories {
    /// Create with a single shared connection (suitable for unit tests).
    /// In production, prefer `open()` which gives each repo its own connection.
    pub fn new(conn: Connection) -> Self {
        let conn = Arc::new(conn);
        Self {
            projects: ProjectsRepository::new(Arc::clone(&conn)),
            loops: LoopsRepository::new(Arc::clone(&conn)),
            runs: RunsRepository::new(Arc::clone(&conn)),
            agent_executions: AgentExecutionsRepository::new(Arc::clone(&conn)),
            pull_request_snapshots: PullRequestSnapshotsRepository::new(Arc::clone(&conn)),
            events: EventsRepository::new(Arc::clone(&conn)),
            locks: LocksRepository::new(Arc::clone(&conn)),
            queue: QueueRepository::new(Arc::clone(&conn)),
            notifications: NotificationsRepository::new(Arc::clone(&conn)),
            worktrees: WorktreesRepository::new(Arc::clone(&conn)),
            webhook_forwarders: WebhookForwardersRepository::new(Arc::clone(&conn)),
            webhook_tunnel_hooks: WebhookTunnelHooksRepository::new(Arc::clone(&conn)),
        }
    }

    /// Open a new set of repositories, each with its own dedicated SQLite
    /// connection. This avoids sharing a single `rusqlite::Connection` across
    /// repos — since `Connection` wraps `RefCell<InnerConnection>`, sharing it
    /// via `Arc` causes runtime panics when concurrent (or nested sequential)
    /// accesses try to borrow the `RefCell`.
    pub fn open(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let make_conn = || -> Result<Arc<Connection>, Box<dyn std::error::Error>> {
            let conn = Connection::open(path)?;
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
            Ok(Arc::new(conn))
        };
        Ok(Self {
            projects: ProjectsRepository::new(make_conn()?),
            loops: LoopsRepository::new(make_conn()?),
            runs: RunsRepository::new(make_conn()?),
            agent_executions: AgentExecutionsRepository::new(make_conn()?),
            pull_request_snapshots: PullRequestSnapshotsRepository::new(make_conn()?),
            events: EventsRepository::new(make_conn()?),
            locks: LocksRepository::new(make_conn()?),
            queue: QueueRepository::new(make_conn()?),
            notifications: NotificationsRepository::new(make_conn()?),
            worktrees: WorktreesRepository::new(make_conn()?),
            webhook_forwarders: WebhookForwardersRepository::new(make_conn()?),
            webhook_tunnel_hooks: WebhookTunnelHooksRepository::new(make_conn()?),
        })
    }
}
