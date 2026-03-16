use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use chrono::{NaiveDateTime, TimeZone, Utc};
use grove_br::{
    BeadCacheStore, BrDependencySnapshot, BrIssueSummary, CachedBeadState, UpsertOutcome,
};
use grove_types::{
    BeadId, BeadPriority, BeadRef, FailureClass, GroveBeadRecord, GroveBeadStatus, RunId, Timestamp,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde_json::Value;

pub const CRATE_PURPOSE: &str = "SQLite bootstrap, migrations, and runtime persistence.";

const PRAGMAS: &[&str] = &[
    "PRAGMA journal_mode = WAL;",
    "PRAGMA foreign_keys = ON;",
    "PRAGMA synchronous = NORMAL;",
    "PRAGMA temp_store = MEMORY;",
    "PRAGMA busy_timeout = 5000;",
];

const MIGRATION_MANIFEST: &[Migration<'_>] = &[Migration {
    version: 1,
    name: "0001_init.sql",
    sql: include_str!("../migrations/0001_init.sql"),
}];

#[derive(Debug)]
pub struct Database {
    conn: Connection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationState {
    pub version: i64,
    pub name: String,
}

#[derive(Debug, Clone, Copy)]
struct Migration<'a> {
    version: i64,
    name: &'a str,
    sql: &'a str,
}

#[derive(Debug)]
struct RawBeadRecordRow {
    bead_id: String,
    title: String,
    description: Option<String>,
    priority: i64,
    issue_type: String,
    br_status: String,
    assignee: Option<String>,
    labels_json: String,
    raw_json: String,
    synced_at: String,
    grove_status: Option<String>,
    declared_paths_json: Option<String>,
    metadata_json: Option<String>,
    last_run_id: Option<String>,
    retry_after: Option<String>,
    last_failure_class: Option<String>,
    last_failure_detail: Option<String>,
    runtime_updated_at: Option<String>,
}

impl Database {
    pub fn open(path: &Utf8Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create database parent directory: {parent}"))?;
        }

        let connection = Connection::open_with_flags(
            utf8_to_std_path(path)?,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .with_context(|| format!("open SQLite database at {path}"))?;

        apply_pragmas(&connection)?;

        Ok(Self { conn: connection })
    }

    pub fn migrate(&mut self) -> Result<()> {
        ensure_migration_table(&self.conn)?;

        for migration in MIGRATION_MANIFEST {
            let applied_name = self.applied_migration_name(migration.version)?;
            match applied_name {
                Some(existing_name) if existing_name == migration.name => continue,
                Some(existing_name) => {
                    bail!(
                        "migration version {} already applied with different name: {} != {}",
                        migration.version,
                        existing_name,
                        migration.name
                    );
                }
                None => self.apply_migration(*migration)?,
            }
        }

        Ok(())
    }

    pub fn with_tx<T>(&mut self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let tx = self.conn.transaction().context("begin transaction")?;
        let value = f(&tx)?;
        tx.commit().context("commit transaction")?;
        Ok(value)
    }

    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn applied_migrations(&self) -> Result<Vec<MigrationState>> {
        let mut stmt = self
            .conn
            .prepare("SELECT version, name FROM _migrations ORDER BY version")
            .context("prepare applied migrations query")?;

        let rows = stmt
            .query_map([], |row| {
                let version = row.get(0)?;
                let name: String = row.get(1)?;
                Ok((version, name))
            })
            .context("query applied migrations")?;

        let pairs = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect applied migrations")?;

        Ok(pairs
            .into_iter()
            .map(|(version, name)| MigrationState { version, name })
            .collect())
    }

    pub fn list_bead_records(&self) -> Result<Vec<GroveBeadRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.bead_id, c.title, c.description, c.priority, c.issue_type, c.status, c.assignee, \
                    c.labels_json, c.raw_json, c.synced_at, r.grove_status, r.declared_paths_json, \
                    r.metadata_json, r.last_run_id, r.retry_after, r.last_failure_class, \
                    r.last_failure_detail, r.runtime_updated_at \
                 FROM bead_cache c \
                 LEFT JOIN bead_runtime r ON r.bead_id = c.bead_id \
                 ORDER BY c.priority ASC, c.bead_id ASC",
            )
            .context("prepare bead record list query")?;

        let rows = stmt
            .query_map([], raw_bead_record_row)
            .context("query bead records")?;

        let raw_rows = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect bead records")?;

        raw_rows
            .into_iter()
            .map(raw_bead_record_into_record)
            .collect()
    }

    pub fn get_bead_record(&self, bead_id: &BeadId) -> Result<Option<GroveBeadRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.bead_id, c.title, c.description, c.priority, c.issue_type, c.status, c.assignee, \
                    c.labels_json, c.raw_json, c.synced_at, r.grove_status, r.declared_paths_json, \
                    r.metadata_json, r.last_run_id, r.retry_after, r.last_failure_class, \
                    r.last_failure_detail, r.runtime_updated_at \
                 FROM bead_cache c \
                 LEFT JOIN bead_runtime r ON r.bead_id = c.bead_id \
                 WHERE c.bead_id = ?1",
            )
            .context("prepare single bead record query")?;

        let raw = stmt
            .query_row([bead_id.as_str()], raw_bead_record_row)
            .optional()
            .with_context(|| format!("query bead record for {}", bead_id.as_str()))?;

        raw.map(raw_bead_record_into_record).transpose()
    }

    pub fn dependency_snapshot(&self, bead_id: &BeadId) -> Result<BrDependencySnapshot> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT parent_id, child_id \
                 FROM bead_dependencies \
                 WHERE relation_type = 'blocks' \
                   AND (parent_id = ?1 OR child_id = ?1) \
                 ORDER BY parent_id, child_id",
            )
            .context("prepare dependency snapshot query")?;

        let rows = stmt
            .query_map([bead_id.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .with_context(|| format!("query dependency snapshot for {}", bead_id.as_str()))?;

        let edges = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect dependency snapshot rows")?;

        let mut blocked_by = Vec::new();
        let mut blocks = Vec::new();

        for (parent_id, child_id) in edges {
            if child_id == bead_id.as_str() {
                blocked_by.push(BeadId::new(parent_id.clone()));
            }
            if parent_id == bead_id.as_str() {
                blocks.push(BeadId::new(child_id));
            }
        }

        Ok(BrDependencySnapshot {
            bead_id: bead_id.clone(),
            blocked_by,
            blocks,
            rows: Vec::new(),
        })
    }

    fn applied_migration_name(&self, version: i64) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT name FROM _migrations WHERE version = ?1",
                [version],
                |row| row.get(0),
            )
            .optional()
            .with_context(|| format!("lookup applied migration version {version}"))
    }

    fn apply_migration(&mut self, migration: Migration<'_>) -> Result<()> {
        let tx = self
            .conn
            .transaction()
            .with_context(|| format!("begin migration {} transaction", migration.name))?;
        tx.execute_batch(migration.sql)
            .with_context(|| format!("execute migration {}", migration.name))?;
        tx.execute(
            "INSERT INTO _migrations(version, name) VALUES (?1, ?2)",
            (migration.version, migration.name),
        )
        .with_context(|| format!("record migration {}", migration.name))?;
        tx.commit()
            .with_context(|| format!("commit migration {}", migration.name))?;
        Ok(())
    }
}

impl BeadCacheStore for Database {
    fn upsert_bead_cache(&mut self, bead: &BrIssueSummary) -> Result<UpsertOutcome> {
        let existed = self
            .conn
            .query_row(
                "SELECT 1 FROM bead_cache WHERE bead_id = ?1",
                [bead.id.as_str()],
                |_| Ok(()),
            )
            .optional()
            .context("check existing bead cache row")?
            .is_some();

        let labels_json = serde_json::to_string(&bead.labels).context("serialize bead labels")?;
        let dependency_ids_json =
            serde_json::to_string(&bead.blocked_by).context("serialize blocked_by ids")?;
        let dependent_ids_json =
            serde_json::to_string(&bead.blocks).context("serialize dependent ids")?;
        let raw_json = serde_json::to_string(&bead.raw_json).context("serialize raw bead JSON")?;
        let synced_at = now_timestamp_string();

        self.conn
            .execute(
                "INSERT INTO bead_cache(\
                    bead_id, title, description, priority, issue_type, status, assignee,\
                    labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, '[]', ?9, ?10, ?11, ?12) \
                ON CONFLICT(bead_id) DO UPDATE SET \
                    title = excluded.title, \
                    description = excluded.description, \
                    priority = excluded.priority, \
                    issue_type = excluded.issue_type, \
                    status = excluded.status, \
                    assignee = excluded.assignee, \
                    labels_json = excluded.labels_json, \
                    parent_ids_json = excluded.parent_ids_json, \
                    dependency_ids_json = excluded.dependency_ids_json, \
                    dependent_ids_json = excluded.dependent_ids_json, \
                    raw_json = excluded.raw_json, \
                    synced_at = excluded.synced_at",
                params![
                    bead.id.as_str(),
                    &bead.title,
                    bead.description.as_deref(),
                    bead_priority_to_db(bead.priority),
                    &bead.issue_type,
                    &bead.status,
                    bead.assignee.as_deref(),
                    &labels_json,
                    &dependency_ids_json,
                    &dependent_ids_json,
                    &raw_json,
                    &synced_at,
                ],
            )
            .with_context(|| format!("upsert bead cache row for {}", bead.id.as_str()))?;

        Ok(if existed {
            UpsertOutcome::Updated
        } else {
            UpsertOutcome::Added
        })
    }

    fn replace_dependency_snapshot(
        &mut self,
        bead_id: &BeadId,
        blocked_by: &[BeadId],
        blocks: &[BeadId],
    ) -> Result<()> {
        let tx = self.conn.transaction().with_context(|| {
            format!("begin dependency snapshot update for {}", bead_id.as_str())
        })?;

        tx.execute(
            "DELETE FROM bead_dependencies \
             WHERE relation_type = 'blocks' \
               AND (parent_id = ?1 OR child_id = ?1)",
            [bead_id.as_str()],
        )
        .with_context(|| format!("clear dependency snapshot for {}", bead_id.as_str()))?;

        let synced_at = now_timestamp_string();
        for dependency_id in blocked_by {
            tx.execute(
                "INSERT INTO bead_dependencies(parent_id, child_id, relation_type, synced_at) \
                 VALUES (?1, ?2, 'blocks', ?3)",
                params![dependency_id.as_str(), bead_id.as_str(), &synced_at],
            )
            .with_context(|| {
                format!(
                    "insert blocking dependency edge {} -> {}",
                    dependency_id.as_str(),
                    bead_id.as_str()
                )
            })?;
        }

        for dependent_id in blocks {
            tx.execute(
                "INSERT INTO bead_dependencies(parent_id, child_id, relation_type, synced_at) \
                 VALUES (?1, ?2, 'blocks', ?3)",
                params![bead_id.as_str(), dependent_id.as_str(), &synced_at],
            )
            .with_context(|| {
                format!(
                    "insert dependent edge {} -> {}",
                    bead_id.as_str(),
                    dependent_id.as_str()
                )
            })?;
        }

        tx.commit()
            .with_context(|| format!("commit dependency snapshot for {}", bead_id.as_str()))?;
        Ok(())
    }

    fn list_cached_beads(&self) -> Result<Vec<CachedBeadState>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.bead_id, r.grove_status \
                 FROM bead_cache c \
                 LEFT JOIN bead_runtime r ON r.bead_id = c.bead_id \
                 ORDER BY c.bead_id ASC",
            )
            .context("prepare cached bead list query")?;

        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .context("query cached bead states")?;

        let entries = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect cached bead states")?;

        entries
            .into_iter()
            .map(|(bead_id, grove_status)| {
                Ok(CachedBeadState {
                    bead_id: BeadId::new(bead_id),
                    grove_status: grove_status
                        .as_deref()
                        .map(parse_grove_bead_status)
                        .transpose()?,
                })
            })
            .collect()
    }

    fn set_grove_status(&mut self, bead_id: &BeadId, status: GroveBeadStatus) -> Result<()> {
        let runtime_updated_at = now_timestamp_string();
        self.conn
            .execute(
                "INSERT INTO bead_runtime(\
                    bead_id, grove_status, declared_paths_json, metadata_json, last_run_id, retry_after,\
                    last_failure_class, last_failure_detail, runtime_updated_at\
                 ) VALUES (?1, ?2, '[]', '{}', NULL, NULL, NULL, NULL, ?3) \
                 ON CONFLICT(bead_id) DO UPDATE SET \
                    grove_status = excluded.grove_status, \
                    runtime_updated_at = excluded.runtime_updated_at",
                params![
                    bead_id.as_str(),
                    encode_grove_bead_status(status),
                    &runtime_updated_at,
                ],
            )
            .with_context(|| format!("set grove status for {}", bead_id.as_str()))?;
        Ok(())
    }
}

pub fn migrations_dir() -> &'static str {
    "migrations"
}

fn apply_pragmas(conn: &Connection) -> Result<()> {
    for pragma in PRAGMAS {
        conn.execute_batch(pragma)
            .with_context(|| format!("apply SQLite pragma {pragma}"))?;
    }
    Ok(())
}

fn ensure_migration_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _migrations (\
            version INTEGER PRIMARY KEY,\
            name TEXT NOT NULL,\
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP\
        );",
    )
    .context("ensure _migrations table exists")?;
    Ok(())
}

fn utf8_to_std_path(path: &Utf8Path) -> Result<PathBuf> {
    let std_path = Utf8PathBuf::from(path).into_std_path_buf();
    if std_path.as_os_str().is_empty() {
        bail!("database path resolved to an empty path from {path}");
    }
    Ok(std_path)
}

fn raw_bead_record_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawBeadRecordRow> {
    Ok(RawBeadRecordRow {
        bead_id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        priority: row.get(3)?,
        issue_type: row.get(4)?,
        br_status: row.get(5)?,
        assignee: row.get(6)?,
        labels_json: row.get(7)?,
        raw_json: row.get(8)?,
        synced_at: row.get(9)?,
        grove_status: row.get(10)?,
        declared_paths_json: row.get(11)?,
        metadata_json: row.get(12)?,
        last_run_id: row.get(13)?,
        retry_after: row.get(14)?,
        last_failure_class: row.get(15)?,
        last_failure_detail: row.get(16)?,
        runtime_updated_at: row.get(17)?,
    })
}

fn raw_bead_record_into_record(row: RawBeadRecordRow) -> Result<GroveBeadRecord> {
    let raw_json: Value = parse_json(&row.raw_json, "raw bead JSON")?;
    let synced_at = parse_timestamp(&row.synced_at)?;
    let created_at = raw_issue_timestamp(&raw_json, "created_at")?.unwrap_or(synced_at);
    let updated_at = raw_issue_timestamp(&raw_json, "updated_at")?.unwrap_or(created_at);
    let runtime_updated_at = row
        .runtime_updated_at
        .as_deref()
        .map(parse_timestamp)
        .transpose()?
        .unwrap_or(updated_at);

    Ok(GroveBeadRecord {
        bead: BeadRef {
            id: BeadId::new(row.bead_id),
            title: row.title,
            description: row.description,
            priority: parse_bead_priority(row.priority)?,
            issue_type: row.issue_type,
            br_status: row.br_status,
            assignee: row.assignee,
            labels: parse_json(&row.labels_json, "bead labels")?,
            created_at,
            updated_at,
        },
        grove_status: row
            .grove_status
            .as_deref()
            .map(parse_grove_bead_status)
            .transpose()?
            .unwrap_or(GroveBeadStatus::Idle),
        declared_paths: parse_json(
            row.declared_paths_json.as_deref().unwrap_or("[]"),
            "declared paths",
        )?,
        metadata: parse_json(
            row.metadata_json.as_deref().unwrap_or("{}"),
            "runtime metadata",
        )?,
        last_run_id: row.last_run_id.map(RunId::new),
        retry_after: row
            .retry_after
            .as_deref()
            .map(parse_timestamp)
            .transpose()?,
        last_failure_class: row
            .last_failure_class
            .as_deref()
            .map(parse_failure_class)
            .transpose()?,
        last_failure_detail: row.last_failure_detail,
        synced_at,
        runtime_updated_at,
    })
}

fn parse_json<T: serde::de::DeserializeOwned>(text: &str, label: &str) -> Result<T> {
    serde_json::from_str(text).with_context(|| format!("parse {label} JSON"))
}

fn raw_issue_timestamp(raw_json: &Value, field: &str) -> Result<Option<Timestamp>> {
    raw_json
        .get(field)
        .and_then(Value::as_str)
        .map(parse_timestamp)
        .transpose()
        .with_context(|| format!("parse {field} from raw bead JSON"))
}

fn parse_timestamp(text: &str) -> Result<Timestamp> {
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .or_else(|_| {
            NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S")
                .map(|timestamp| Utc.from_utc_datetime(&timestamp))
        })
        .with_context(|| format!("parse timestamp {text}"))
}

fn now_timestamp_string() -> String {
    Utc::now().to_rfc3339()
}

fn parse_bead_priority(value: i64) -> Result<BeadPriority> {
    match value {
        0 => Ok(BeadPriority::P0),
        1 => Ok(BeadPriority::P1),
        2 => Ok(BeadPriority::P2),
        3 => Ok(BeadPriority::P3),
        4 => Ok(BeadPriority::P4),
        _ => bail!("unsupported bead priority value {value}"),
    }
}

fn bead_priority_to_db(priority: BeadPriority) -> i64 {
    match priority {
        BeadPriority::P0 => 0,
        BeadPriority::P1 => 1,
        BeadPriority::P2 => 2,
        BeadPriority::P3 => 3,
        BeadPriority::P4 => 4,
    }
}

fn encode_grove_bead_status(status: GroveBeadStatus) -> &'static str {
    match status {
        GroveBeadStatus::Idle => "Idle",
        GroveBeadStatus::Ready => "Ready",
        GroveBeadStatus::Running => "Running",
        GroveBeadStatus::Checkpointed => "Checkpointed",
        GroveBeadStatus::WaitingToRetry => "WaitingToRetry",
        GroveBeadStatus::Succeeded => "Succeeded",
        GroveBeadStatus::Failed => "Failed",
    }
}

fn parse_grove_bead_status(text: &str) -> Result<GroveBeadStatus> {
    match normalize_enum_token(text).as_str() {
        "idle" => Ok(GroveBeadStatus::Idle),
        "ready" => Ok(GroveBeadStatus::Ready),
        "running" => Ok(GroveBeadStatus::Running),
        "checkpointed" => Ok(GroveBeadStatus::Checkpointed),
        "waitingtoretry" => Ok(GroveBeadStatus::WaitingToRetry),
        "succeeded" => Ok(GroveBeadStatus::Succeeded),
        "failed" => Ok(GroveBeadStatus::Failed),
        _ => bail!("unsupported grove bead status {text}"),
    }
}

fn parse_failure_class(text: &str) -> Result<FailureClass> {
    match normalize_enum_token(text).as_str() {
        "timeout" => Ok(FailureClass::Timeout),
        "ratelimit" => Ok(FailureClass::RateLimit),
        "permissiondenied" => Ok(FailureClass::PermissionDenied),
        "circuitopen" => Ok(FailureClass::CircuitOpen),
        "noprogress" => Ok(FailureClass::NoProgress),
        "repeatederror" => Ok(FailureClass::RepeatedError),
        "protocolmalformed" => Ok(FailureClass::ProtocolMalformed),
        "claudecrashed" => Ok(FailureClass::ClaudeCrashed),
        "brmirrorfailed" => Ok(FailureClass::BrMirrorFailed),
        "interrupted" => Ok(FailureClass::Interrupted),
        "unknown" => Ok(FailureClass::Unknown),
        _ => bail!("unsupported failure class {text}"),
    }
}

fn normalize_enum_token(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use camino::Utf8PathBuf;
    use grove_br::{
        BeadCacheStore, BrCapability, BrClient, BrDependencySnapshot, BrError, BrIssueDetail,
        BrIssueSummary, BrVersion, sync_bead_cache,
    };
    use grove_types::{BeadId, BeadPriority, Timestamp};
    use rusqlite::OptionalExtension;
    use serde_json::json;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    use super::{CachedBeadState, Database, GroveBeadStatus};
    use crate::MigrationState;

    #[test]
    fn open_creates_database_parent_directory() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("nested/.grove/grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;

        let _db = Database::open(&db_path)?;

        assert!(db_path.exists());
        Ok(())
    }

    #[test]
    fn migrate_applies_manifest_once() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;

        db.migrate()?;
        db.migrate()?;

        let migrations = db.applied_migrations()?;
        assert_eq!(migrations.len(), 1);
        assert_eq!(
            migrations[0],
            MigrationState {
                version: 1,
                name: "0001_init.sql".into(),
            }
        );
        Ok(())
    }

    #[test]
    fn migrate_creates_runtime_tables() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;

        db.migrate()?;

        for table in [
            "_migrations",
            "bead_cache",
            "bead_runtime",
            "bead_dependencies",
            "task_runs",
            "claude_sessions",
            "checkpoints",
            "handoffs",
            "reservations",
            "event_log",
        ] {
            let exists: Option<String> = db
                .connection()
                .query_row(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .optional()?;
            assert_eq!(exists.as_deref(), Some(table));
        }

        Ok(())
    }

    #[test]
    fn with_tx_commits_changes() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        db.with_tx(|tx| {
            tx.execute(
                "INSERT INTO bead_cache(\
                    bead_id, title, description, priority, issue_type, status, assignee,\
                    labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json,\
                    raw_json, synced_at\
                ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, '[]', '[]', '[]', '[]', ?6, CURRENT_TIMESTAMP)",
                (
                    "grove-123",
                    "Example bead",
                    0,
                    "task",
                    "open",
                    "{}",
                ),
            )?;
            Ok(())
        })?;

        let count: i64 =
            db.connection()
                .query_row("SELECT COUNT(*) FROM bead_cache", [], |row| row.get(0))?;
        assert_eq!(count, 1);
        Ok(())
    }

    #[test]
    fn sync_bead_cache_populates_database_records_and_runtime_state() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        let bead = sample_issue(
            "grove-1j9.5.7",
            "kernel services",
            vec![BeadId::new("grove-1j9.5.4")],
            vec![BeadId::new("grove-1j9.5.8")],
        )?;
        let br = FakeBrClient {
            ready: vec![bead.clone()],
            list_open: vec![bead.clone()],
            dep_snapshots: BTreeMap::from([(
                bead.id.as_str().to_owned(),
                bead.dependency_snapshot(),
            )]),
        };

        let first = sync_bead_cache(&br, &mut db)?;
        let second = sync_bead_cache(&br, &mut db)?;

        assert_eq!(first.beads_added, 1, "first sync result: {first:?}");
        assert_eq!(second.beads_updated, 1);
        assert!(first.errors.is_empty());
        assert!(second.errors.is_empty());

        let cached = db.list_cached_beads()?;
        assert_eq!(
            cached,
            vec![CachedBeadState {
                bead_id: bead.id.clone(),
                grove_status: Some(GroveBeadStatus::Ready),
            }]
        );

        let Some(record) = db.get_bead_record(&bead.id)? else {
            anyhow::bail!("record should exist");
        };
        assert_eq!(record.bead.id, bead.id);
        assert_eq!(record.bead.title, bead.title);
        assert_eq!(record.bead.priority, bead.priority);
        assert_eq!(record.bead.created_at, bead.created_at);
        assert_eq!(record.bead.updated_at, bead.updated_at);
        assert_eq!(record.grove_status, GroveBeadStatus::Ready);
        assert!(record.declared_paths.is_empty());
        assert_eq!(record.metadata, json!({}));

        let listed = db.list_bead_records()?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].bead.id, bead.id);

        let snapshot = db.dependency_snapshot(&bead.id)?;
        assert_eq!(snapshot.blocked_by, bead.blocked_by);
        assert_eq!(snapshot.blocks, bead.blocks);
        Ok(())
    }

    #[test]
    fn get_bead_record_defaults_runtime_fields_and_parses_sqlite_timestamps() -> Result<()> {
        let dir = tempdir()?;
        let db_path = Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
            .map_err(|_| anyhow::anyhow!("temp path was not valid UTF-8"))?;
        let mut db = Database::open(&db_path)?;
        db.migrate()?;

        db.connection().execute(
            "INSERT INTO bead_cache(\
                bead_id, title, description, priority, issue_type, status, assignee,\
                labels_json, parent_ids_json, dependency_ids_json, dependent_ids_json, raw_json, synced_at\
            ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, ?6, '[]', '[]', '[]', ?7, CURRENT_TIMESTAMP)",
            rusqlite::params![
                "grove-fallback",
                "Fallback bead",
                1,
                "task",
                "open",
                "[\"area:test\"]",
                "{\"id\":\"grove-fallback\"}",
            ],
        )?;

        let Some(record) = db.get_bead_record(&BeadId::new("grove-fallback"))? else {
            anyhow::bail!("fallback bead should exist");
        };

        assert_eq!(record.grove_status, GroveBeadStatus::Idle);
        assert!(record.declared_paths.is_empty());
        assert_eq!(record.metadata, json!({}));
        assert_eq!(record.bead.created_at, record.synced_at);
        assert_eq!(record.bead.updated_at, record.bead.created_at);
        Ok(())
    }

    struct FakeBrClient {
        ready: Vec<BrIssueSummary>,
        list_open: Vec<BrIssueSummary>,
        dep_snapshots: BTreeMap<String, BrDependencySnapshot>,
    }

    impl BrClient for FakeBrClient {
        fn ready(&self) -> Result<Vec<BrIssueSummary>, BrError> {
            Ok(self.ready.clone())
        }

        fn list_open(&self) -> Result<Vec<BrIssueSummary>, BrError> {
            Ok(self.list_open.clone())
        }

        fn show(&self, id: &BeadId) -> Result<BrIssueDetail, BrError> {
            Err(BrError::BeadNotFound { id: id.clone() })
        }

        fn dep_list(&self, id: &BeadId) -> Result<BrDependencySnapshot, BrError> {
            self.dep_snapshots
                .get(id.as_str())
                .cloned()
                .ok_or_else(|| BrError::BeadNotFound { id: id.clone() })
        }

        fn capability(&self) -> Result<BrCapability, BrError> {
            Ok(BrCapability {
                available: true,
                version_line: Some("br 0.1.12".into()),
                version: Some(BrVersion {
                    raw: "br 0.1.12".into(),
                    major: Some(0),
                    minor: Some(1),
                    patch: Some(12),
                }),
                beads_dir_exists: true,
            })
        }
    }

    fn sample_issue(
        id: &str,
        title: &str,
        blocked_by: Vec<BeadId>,
        blocks: Vec<BeadId>,
    ) -> Result<BrIssueSummary> {
        let created_at: Timestamp = "2026-03-16T10:00:00Z".parse()?;
        let updated_at: Timestamp = "2026-03-16T11:00:00Z".parse()?;

        Ok(BrIssueSummary {
            id: BeadId::new(id),
            title: title.into(),
            description: Some(format!("description for {title}")),
            priority: BeadPriority::P1,
            issue_type: "task".into(),
            status: "open".into(),
            assignee: None,
            labels: vec!["area:test".into()],
            created_at,
            updated_at,
            blocked_by,
            blocks,
            raw_json: json!({
                "id": id,
                "title": title,
                "created_at": created_at.to_rfc3339(),
                "updated_at": updated_at.to_rfc3339(),
            }),
        })
    }
}
