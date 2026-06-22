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
}
