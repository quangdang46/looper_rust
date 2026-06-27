use refinery::Migration;
use refinery::Runner;
use rusqlite::Connection;
use tracing::info;

use crate::error::{Result, StorageError};

/// Build migration list manually to avoid ordering bugs in
/// `embed_migrations!` macro (which can shuffle content when files
/// are added/removed across compilations).
fn get_migrations() -> Vec<Migration> {
    vec![
        Migration::unapplied("V1__initial", include_str!("../migrations/V1__initial.sql")).unwrap(),
        Migration::unapplied("V2__extend_coverage", include_str!("../migrations/V2__extend_coverage.sql")).unwrap(),
        Migration::unapplied("V3__runs_agent_columns", include_str!("../migrations/V3__runs_agent_columns.sql"))
            .unwrap(),
        Migration::unapplied("V4__outcomes", include_str!("../migrations/V4__outcomes.sql")).unwrap(),
    ]
}

/// Run all pending migrations against the given SQLite connection.
/// Returns an error if any migration fails, which should prevent service startup.
pub fn run_migrations(conn: &mut Connection) -> Result<()> {
    let migrations = get_migrations();
    let runner = Runner::new(&migrations);
    let report = runner.run(conn).map_err(|e| StorageError::Migration(format!("refinery migration failed: {e}")))?;

    info!(
        "Applied {} migration(s): {:?}",
        report.applied_migrations().len(),
        report.applied_migrations().iter().map(|m| m.version().to_string()).collect::<Vec<_>>()
    );

    Ok(())
}
