use refinery::embed_migrations;
use rusqlite::Connection;
use tracing::info;

use crate::error::{Result, StorageError};

embed_migrations!("migrations");

/// Run all pending migrations against the given SQLite connection.
/// Returns an error if any migration fails, which should prevent service startup.
pub fn run_migrations(conn: &mut Connection) -> Result<()> {
    let report = migrations::runner()
        .run(conn)
        .map_err(|e| StorageError::Migration(format!("refinery migration failed: {e}")))?;

    info!(
        "Applied {} migration(s): {:?}",
        report.applied_migrations().len(),
        report
            .applied_migrations()
            .iter()
            .map(|m| m.version().to_string())
            .collect::<Vec<_>>()
    );

    Ok(())
}
