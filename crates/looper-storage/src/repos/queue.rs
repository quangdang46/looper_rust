use std::collections::HashMap;
use std::sync::Arc;

use crate::error::{Result, StorageError};
use crate::helpers::{chunk_strings, is_queue_active_dedupe_constraint_error, sql_placeholders, SQLITE_MAX_VARIABLES};
use crate::record::{QueueFailInput, QueueItemRecord, QueueMarkRetryInput, QueueStats};
use rusqlite::Connection;

fn scan_queue_item(row: &rusqlite::Row) -> rusqlite::Result<QueueItemRecord> {
    Ok(QueueItemRecord {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        loop_id: row.get("loop_id")?,
        r#type: row.get("type")?,
        target_type: row.get("target_type")?,
        target_id: row.get("target_id")?,
        repo: row.get("repo")?,
        pr_number: row.get("pr_number")?,
        dedupe_key: row.get("dedupe_key")?,
        priority: row.get("priority")?,
        status: row.get("status")?,
        available_at: row.get("available_at")?,
        attempts: row.get("attempts")?,
        max_attempts: row.get("max_attempts")?,
        claimed_by: row.get("claimed_by")?,
        claimed_at: row.get("claimed_at")?,
        started_at: row.get("started_at")?,
        finished_at: row.get("finished_at")?,
        lock_key: row.get("lock_key")?,
        payload_json: row.get("payload_json")?,
        last_error: row.get("last_error")?,
        last_error_kind: row.get("last_error_kind")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

const QUEUE_COLUMNS: &str =
    "id, project_id, loop_id, type, target_type, target_id, repo, pr_number, dedupe_key, priority, status, available_at, attempts, max_attempts, claimed_by, claimed_at, started_at, finished_at, lock_key, payload_json, last_error, last_error_kind, created_at, updated_at";

pub const QUEUE_LONG_TERM_RETRY_ATTEMPT_THRESHOLD: i64 = 5;

/// Build the WHERE clause fragment for the scheduled queue blocking conditions.
/// Used in list_scheduled, claim_next variants, and stats.
fn scheduled_queue_conditions() -> &'static str {
    "qi.status = 'queued'
     AND qi.available_at <= ?1
     AND (qi.lock_key IS NULL OR NOT EXISTS (SELECT 1 FROM locks l WHERE l.key = qi.lock_key AND l.expires_at > ?1))
     AND NOT (qi.type = 'fixer' AND EXISTS (SELECT 1 FROM queue_items qir WHERE qir.loop_id = qi.loop_id AND qir.type = 'reviewer' AND qir.status IN ('queued', 'running')))
     AND NOT EXISTS (SELECT 1 FROM projects p WHERE p.id = qi.project_id AND p.archived = 1)
     AND NOT EXISTS (SELECT 1 FROM loops l WHERE l.id = qi.loop_id AND l.status IN ('completed', 'cancelled', 'failed', 'paused'))"
}

#[derive(Clone)]
pub struct QueueRepository {
    conn: Arc<Connection>,
}

impl QueueRepository {
    pub fn new(conn: Arc<Connection>) -> Self {
        Self { conn }
    }

    pub fn find_active_by_dedupe(&self, dedupe_key: &str) -> Result<Option<QueueItemRecord>> {
        let sql = format!(
            "SELECT {} FROM queue_items WHERE dedupe_key=?1 AND status IN ('queued','running') LIMIT 1",
            QUEUE_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![dedupe_key], scan_queue_item)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn find_active_by_loop_id(&self, loop_id: &str) -> Result<Option<QueueItemRecord>> {
        let sql = format!(
            "SELECT {} FROM queue_items WHERE loop_id=?1 AND status IN ('queued','running') LIMIT 1",
            QUEUE_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![loop_id], scan_queue_item)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Insert a queue item, or return the existing active row with the same
    /// `dedupe_key` (`status IN ('queued','running')`).
    ///
    /// Uses a lookup first for types without a race window concern, then
    /// INSERT; concurrent races are resolved via the partial unique index
    /// `idx_queue_items_one_active_dedupe` (planner|reviewer|worker|fixer).
    pub fn create_or_get_active_by_dedupe(&self, record: &QueueItemRecord) -> Result<(QueueItemRecord, bool)> {
        if let Some(existing) = self.find_active_by_dedupe(&record.dedupe_key)? {
            return Ok((existing, false));
        }

        match self.conn.execute(
            "INSERT INTO queue_items (id, project_id, loop_id, type, target_type, target_id, repo, pr_number, dedupe_key, priority, status, available_at, attempts, max_attempts, claimed_by, claimed_at, started_at, finished_at, lock_key, payload_json, last_error, last_error_kind, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
            rusqlite::params![
                &record.id, &record.project_id, &record.loop_id, &record.r#type,
                &record.target_type, &record.target_id, &record.repo, &record.pr_number,
                &record.dedupe_key, &record.priority, &record.status, &record.available_at,
                &record.attempts, &record.max_attempts, &record.claimed_by, &record.claimed_at,
                &record.started_at, &record.finished_at, &record.lock_key, &record.payload_json,
                &record.last_error, &record.last_error_kind, &record.created_at, &record.updated_at,
            ],
        ) {
            Ok(_) => {
                let inserted = self.get_by_id(&record.id)?.ok_or_else(|| {
                    StorageError::NotFound(format!("queue item not found after insert: {}", record.id))
                })?;
                Ok((inserted, true))
            }
            Err(e) if is_queue_active_dedupe_constraint_error(&e) => {
                let existing = self.find_active_by_dedupe(&record.dedupe_key)?.ok_or_else(|| {
                    StorageError::NotFound(format!("active queue item not found for dedupe_key: {}", record.dedupe_key))
                })?;
                Ok((existing, false))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn upsert_active_by_dedupe_or_get_existing(&self, record: &QueueItemRecord) -> Result<(QueueItemRecord, bool)> {
        match self.conn.execute(
            "INSERT INTO queue_items (id, project_id, loop_id, type, target_type, target_id, repo, pr_number, dedupe_key, priority, status, available_at, attempts, max_attempts, claimed_by, claimed_at, started_at, finished_at, lock_key, payload_json, last_error, last_error_kind, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)
             ON CONFLICT(dedupe_key) WHERE type IN ('planner','reviewer','worker','fixer') AND status IN ('queued','running') DO UPDATE SET
               project_id=excluded.project_id, loop_id=excluded.loop_id, type=excluded.type,
               target_type=excluded.target_type, target_id=excluded.target_id, repo=excluded.repo,
               pr_number=excluded.pr_number, priority=excluded.priority, status=excluded.status,
               available_at=excluded.available_at, attempts=excluded.attempts, max_attempts=excluded.max_attempts,
               claimed_by=excluded.claimed_by, claimed_at=excluded.claimed_at, started_at=excluded.started_at,
               finished_at=excluded.finished_at, lock_key=excluded.lock_key, payload_json=excluded.payload_json,
               last_error=excluded.last_error, last_error_kind=excluded.last_error_kind, updated_at=excluded.updated_at",
            rusqlite::params![
                &record.id, &record.project_id, &record.loop_id, &record.r#type,
                &record.target_type, &record.target_id, &record.repo, &record.pr_number,
                &record.dedupe_key, &record.priority, &record.status, &record.available_at,
                &record.attempts, &record.max_attempts, &record.claimed_by, &record.claimed_at,
                &record.started_at, &record.finished_at, &record.lock_key, &record.payload_json,
                &record.last_error, &record.last_error_kind, &record.created_at, &record.updated_at,
            ],
        ) {
            Ok(rows) if rows > 0 => {
                // ON CONFLICT can either insert a new row OR update an
                // existing one (when the partial unique index fires).
                // The new record.id only exists if INSERT actually
                // happened; if it doesn't, fall back to dedupe lookup
                // so callers always get a QueueItemRecord back.
                if let Some(inserted) = self.get_by_id(&record.id)? {
                    return Ok((inserted, true));
                }
                let existing = self.find_active_by_dedupe(&record.dedupe_key)?.ok_or_else(|| {
                    StorageError::NotFound(format!(
                        "queue item not found after upsert: id={}, dedupe_key={}",
                        record.id, record.dedupe_key,
                    ))
                })?;
                Ok((existing, false))
            }
            Ok(_) => {
                let existing = self.find_active_by_dedupe(&record.dedupe_key)?.ok_or_else(|| {
                    StorageError::NotFound(format!("active queue item not found for dedupe_key: {}", record.dedupe_key))
                })?;
                Ok((existing, false))
            }
            Err(e) if is_queue_active_dedupe_constraint_error(&e) => {
                let existing = self.find_active_by_dedupe(&record.dedupe_key)?.ok_or_else(|| {
                    StorageError::NotFound(format!("active queue item not found for dedupe_key: {}", record.dedupe_key))
                })?;
                Ok((existing, false))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn upsert(&self, record: &QueueItemRecord) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO queue_items (id, project_id, loop_id, type, target_type, target_id, repo, pr_number, dedupe_key, priority, status, available_at, attempts, max_attempts, claimed_by, claimed_at, started_at, finished_at, lock_key, payload_json, last_error, last_error_kind, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
            rusqlite::params![
                &record.id, &record.project_id, &record.loop_id, &record.r#type,
                &record.target_type, &record.target_id, &record.repo, &record.pr_number,
                &record.dedupe_key, &record.priority, &record.status, &record.available_at,
                &record.attempts, &record.max_attempts, &record.claimed_by, &record.claimed_at,
                &record.started_at, &record.finished_at, &record.lock_key, &record.payload_json,
                &record.last_error, &record.last_error_kind, &record.created_at, &record.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<QueueItemRecord>> {
        let sql = format!("SELECT {} FROM queue_items WHERE id = ?1", QUEUE_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![id], scan_queue_item)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn get_latest_by_loop_id(&self, loop_id: &str) -> Result<Option<QueueItemRecord>> {
        let sql =
            format!("SELECT {} FROM queue_items WHERE loop_id = ?1 ORDER BY created_at DESC LIMIT 1", QUEUE_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![loop_id], scan_queue_item)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn list(&self) -> Result<Vec<QueueItemRecord>> {
        let sql = format!("SELECT {} FROM queue_items ORDER BY created_at DESC", QUEUE_COLUMNS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], scan_queue_item)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn list_by_statuses(&self, statuses: &[String]) -> Result<Vec<QueueItemRecord>> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let chunks = chunk_strings(statuses, SQLITE_MAX_VARIABLES);
        let mut results = Vec::new();
        for chunk in &chunks {
            let inside = sql_placeholders(chunk.len());
            let sql = format!(
                "SELECT {} FROM queue_items WHERE status IN ({}) ORDER BY created_at DESC",
                QUEUE_COLUMNS, inside,
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter().map(|s| s.as_str())), scan_queue_item)?;
            for row in rows {
                results.push(row?);
            }
        }
        Ok(results)
    }

    pub fn list_latest_by_loop_statuses(&self, statuses: &[String]) -> Result<Vec<QueueItemRecord>> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let chunks = chunk_strings(statuses, SQLITE_MAX_VARIABLES);
        let mut results = Vec::new();
        for chunk in &chunks {
            let inside = sql_placeholders(chunk.len());
            let sql = format!(
                "SELECT qi.* FROM queue_items qi
                 INNER JOIN (
                     SELECT loop_id, MAX(created_at) AS max_created_at
                     FROM queue_items
                     WHERE loop_id IS NOT NULL
                     GROUP BY loop_id
                 ) latest ON qi.loop_id = latest.loop_id AND qi.created_at = latest.max_created_at
                 WHERE qi.loop_id IN (SELECT id FROM loops WHERE status IN ({}))",
                inside,
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter().map(|s| s.as_str())), scan_queue_item)?;
            for row in rows {
                results.push(row?);
            }
        }
        Ok(results)
    }

    pub fn count_by_all_statuses(&self) -> Result<HashMap<String, i64>> {
        let mut stmt = self.conn.prepare("SELECT status, COUNT(*) as cnt FROM queue_items GROUP BY status")?;
        let rows = stmt.query_map([], |row| {
            let status: String = row.get("status")?;
            let cnt: i64 = row.get("cnt")?;
            Ok((status, cnt))
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let (status, cnt) = row?;
            map.insert(status, cnt);
        }
        Ok(map)
    }

    pub fn list_queued(&self, limit: i64) -> Result<Vec<QueueItemRecord>> {
        let sql = format!(
            "SELECT {} FROM queue_items WHERE status='queued' ORDER BY priority ASC, available_at ASC, created_at ASC LIMIT ?1",
            QUEUE_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![limit], scan_queue_item)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn count_by_status(&self, status: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM queue_items WHERE status = ?1",
            rusqlite::params![status],
            |row| row.get(0),
        )?)
    }

    pub fn count_active_by_loop_id(&self, loop_id: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM queue_items WHERE loop_id = ?1 AND status IN ('queued','running')",
            rusqlite::params![loop_id],
            |row| row.get(0),
        )?)
    }

    pub fn count_by_loop_id_and_status(&self, loop_id: &str, status: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM queue_items WHERE loop_id = ?1 AND status = ?2",
            rusqlite::params![loop_id, status],
            |row| row.get(0),
        )?)
    }

    pub fn list_scheduled(&self, now_iso: &str, limit: i64) -> Result<Vec<QueueItemRecord>> {
        let sql = format!(
            "SELECT qi.* FROM queue_items qi WHERE {}
             ORDER BY qi.priority ASC, qi.available_at ASC, qi.created_at ASC
             LIMIT ?2",
            scheduled_queue_conditions(),
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![now_iso, limit], scan_queue_item)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn stats(&self, now_iso: &str) -> Result<QueueStats> {
        let total_queued: i64 =
            self.conn.query_row("SELECT COUNT(*) FROM queue_items WHERE status = 'queued'", [], |row| row.get(0))?;

        let eligible_queued: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM queue_items qi WHERE {}", scheduled_queue_conditions(),),
            rusqlite::params![now_iso],
            |row| row.get(0),
        )?;

        let blocked_by_terminal_or_paused_loop: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM queue_items qi
             WHERE qi.status = 'queued'
               AND qi.loop_id IS NOT NULL
               AND EXISTS (SELECT 1 FROM loops l WHERE l.id = qi.loop_id AND l.status IN ('completed', 'cancelled', 'failed', 'paused'))",
            [],
            |row| row.get(0),
        )?;

        let blocked_by_lock_key: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM queue_items qi
             WHERE qi.status = 'queued'
               AND qi.lock_key IS NOT NULL
               AND EXISTS (SELECT 1 FROM locks l WHERE l.key = qi.lock_key AND l.expires_at > ?1)",
            rusqlite::params![now_iso],
            |row| row.get(0),
        )?;

        let blocked_by_reviewer_fixer_dependency: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM queue_items qi
             WHERE qi.status = 'queued'
               AND qi.type = 'fixer'
               AND EXISTS (SELECT 1 FROM queue_items qir WHERE qir.loop_id = qi.loop_id AND qir.type = 'reviewer' AND qir.status IN ('queued', 'running'))",
            [],
            |row| row.get(0),
        )?;

        let scheduled_for_future: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM queue_items WHERE status = 'queued' AND available_at > ?1",
            rusqlite::params![now_iso],
            |row| row.get(0),
        )?;

        let stale_queued: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM queue_items qi
             WHERE qi.status = 'queued'
               AND qi.available_at <= ?1
               AND qi.lock_key IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM locks l WHERE l.key = qi.lock_key AND l.expires_at > ?1)",
            rusqlite::params![now_iso],
            |row| row.get(0),
        )?;

        Ok(QueueStats {
            total_queued,
            eligible_queued,
            blocked_by_terminal_or_paused_loop,
            blocked_by_lock_key,
            blocked_by_reviewer_fixer_dependency,
            scheduled_for_future,
            stale_queued,
        })
    }

    pub fn cleanup_stale_queued(&self, finished_at: &str, reason: &str) -> Result<i64> {
        let rows = self.conn.execute(
            "UPDATE queue_items SET status='cancelled', finished_at=?1, last_error=?2 WHERE status='queued' AND available_at > ?1",
            rusqlite::params![finished_at, reason],
        )?;
        Ok(rows as i64)
    }

    fn claim_next_impl(&self, now_iso: &str, claimed_by: &str, extra_where: &str) -> Result<Option<QueueItemRecord>> {
        let sql = format!(
            "WITH candidate AS (
                SELECT id FROM queue_items qi
                WHERE {}
                  {}
                ORDER BY qi.priority ASC, qi.available_at ASC, qi.created_at ASC
                LIMIT 1
            )
            UPDATE queue_items
            SET status = 'running', claimed_by = ?2, claimed_at = ?3, started_at = COALESCE(started_at, ?4), updated_at = ?5
            WHERE id = (SELECT id FROM candidate) AND status = 'queued'
            RETURNING *",
            scheduled_queue_conditions(),
            extra_where,
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows =
            stmt.query_map(rusqlite::params![now_iso, claimed_by, now_iso, now_iso, now_iso], scan_queue_item)?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn claim_next(&self, now_iso: &str, claimed_by: &str) -> Result<Option<QueueItemRecord>> {
        self.claim_next_impl(now_iso, claimed_by, "")
    }

    pub fn claim_next_non_long_term_retry(&self, now_iso: &str, claimed_by: &str) -> Result<Option<QueueItemRecord>> {
        self.claim_next_impl(
            now_iso,
            claimed_by,
            &format!(" AND (qi.attempts < {} OR qi.last_error_kind IS NULL)", QUEUE_LONG_TERM_RETRY_ATTEMPT_THRESHOLD,),
        )
    }

    pub fn claim_next_long_term_retry(&self, now_iso: &str, claimed_by: &str) -> Result<Option<QueueItemRecord>> {
        self.claim_next_impl(
            now_iso,
            claimed_by,
            &format!(
                " AND qi.attempts >= {} AND qi.last_error_kind IN ('retryable_transient','retryable_after_resume','non_retryable')",
                QUEUE_LONG_TERM_RETRY_ATTEMPT_THRESHOLD,
            ),
        )
    }

    pub fn claim_next_of_type(
        &self,
        now_iso: &str,
        claimed_by: &str,
        queue_type: &str,
    ) -> Result<Option<QueueItemRecord>> {
        let sql = format!(
            "WITH candidate AS (
                SELECT id FROM queue_items qi
                WHERE {}
                  AND qi.type = ?6
                ORDER BY qi.priority ASC, qi.available_at ASC, qi.created_at ASC
                LIMIT 1
            )
            UPDATE queue_items
            SET status = 'running', claimed_by = ?2, claimed_at = ?3, started_at = COALESCE(started_at, ?4), updated_at = ?5
            WHERE id = (SELECT id FROM candidate) AND status = 'queued'
            RETURNING *",
            scheduled_queue_conditions(),
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(
            rusqlite::params![now_iso, claimed_by, now_iso, now_iso, now_iso, queue_type],
            scan_queue_item,
        )?;
        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn complete(&self, id: &str, finished_at: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE queue_items SET status='completed', finished_at=?2, updated_at=?2 WHERE id=?1",
            rusqlite::params![id, finished_at],
        )?;
        Ok(())
    }

    pub fn update_lock_key(&self, id: &str, lock_key: &str, updated_at: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE queue_items SET lock_key=?2, updated_at=?3 WHERE id=?1",
            rusqlite::params![id, lock_key, updated_at],
        )?;
        Ok(())
    }

    pub fn mark_retry(&self, input: &QueueMarkRetryInput) -> Result<()> {
        self.conn.execute(
            "UPDATE queue_items SET status='queued', available_at=?2, attempts=?3, last_error=?4, last_error_kind=?5, updated_at=?6 WHERE id=?1",
            rusqlite::params![&input.id, &input.available_at, &input.attempts, &input.error_message, &input.error_kind, &input.updated_at],
        )?;
        Ok(())
    }

    pub fn fail(&self, input: &QueueFailInput) -> Result<()> {
        self.conn.execute(
            "UPDATE queue_items SET status='manual_intervention', attempts=?2, finished_at=?3, last_error=?4, last_error_kind=?5, updated_at=?6 WHERE id=?1",
            rusqlite::params![&input.id, &input.attempts, &input.finished_at, &input.error_message, &input.error_kind, &input.updated_at],
        )?;
        Ok(())
    }

    pub fn requeue_running_by_loop(&self, loop_id: &str, queued_at: &str) -> Result<i64> {
        let rows = self.conn.execute(
            "UPDATE queue_items SET status='queued', available_at=?2, updated_at=?2 WHERE loop_id=?1 AND status='running'",
            rusqlite::params![loop_id, queued_at],
        )?;
        Ok(rows as i64)
    }

    pub fn requeue_latest_cancelled_by_loop(&self, loop_id: &str, queued_at: &str) -> Result<i64> {
        let rows = self.conn.execute(
            "UPDATE queue_items SET status='queued', available_at=?2, updated_at=?2 WHERE id=(SELECT id FROM queue_items WHERE loop_id=?1 AND status='cancelled' ORDER BY updated_at DESC LIMIT 1)",
            rusqlite::params![loop_id, queued_at],
        )?;
        Ok(rows as i64)
    }

    pub fn requeue_latest_failed_by_loop(&self, loop_id: &str, queued_at: &str) -> Result<i64> {
        let rows = self.conn.execute(
            "UPDATE queue_items SET status='queued', available_at=?2, updated_at=?2 WHERE id=(SELECT id FROM queue_items WHERE loop_id=?1 AND status='failed' ORDER BY updated_at DESC LIMIT 1)",
            rusqlite::params![loop_id, queued_at],
        )?;
        Ok(rows as i64)
    }

    pub fn requeue_failed_by_id(&self, loop_id: &str, queue_id: &str, queued_at: &str) -> Result<i64> {
        let rows = self.conn.execute(
            "UPDATE queue_items SET status='queued', available_at=?3, updated_at=?3 WHERE loop_id=?1 AND id=?2 AND status='failed'",
            rusqlite::params![loop_id, queue_id, queued_at],
        )?;
        Ok(rows as i64)
    }

    pub fn requeue_failed_by_id_with_attempts(
        &self,
        loop_id: &str,
        queue_id: &str,
        queued_at: &str,
        attempts: i64,
    ) -> Result<i64> {
        let rows = self.conn.execute(
            "UPDATE queue_items SET status='queued', available_at=?3, updated_at=?3, attempts=?4 WHERE loop_id=?1 AND id=?2 AND status='failed'",
            rusqlite::params![loop_id, queue_id, queued_at, attempts],
        )?;
        Ok(rows as i64)
    }

    pub fn cancel_by_loop(&self, loop_id: &str, finished_at: &str, reason: Option<&str>) -> Result<i64> {
        let rows = self.conn.execute(
            "UPDATE queue_items SET status='cancelled', finished_at=?2, last_error=?3, updated_at=?2 WHERE loop_id=?1 AND status IN ('queued','running')",
            rusqlite::params![loop_id, finished_at, reason],
        )?;
        Ok(rows as i64)
    }

    pub fn cancel_by_project(&self, project_id: &str, finished_at: &str, reason: Option<&str>) -> Result<i64> {
        let rows = self.conn.execute(
            "UPDATE queue_items SET status='cancelled', finished_at=?2, last_error=?3, updated_at=?2 WHERE project_id=?1 AND status IN ('queued','running')",
            rusqlite::params![project_id, finished_at, reason],
        )?;
        Ok(rows as i64)
    }

    /// Cancel a single active queue item by id. Returns number of rows updated (0 or 1).
    pub fn cancel_by_id(&self, id: &str, finished_at: &str, reason: Option<&str>) -> Result<i64> {
        let rows = self.conn.execute(
            "UPDATE queue_items SET status='cancelled', finished_at=?2, last_error=?3, updated_at=?2 WHERE id=?1 AND status IN ('queued','running')",
            rusqlite::params![id, finished_at, reason],
        )?;
        Ok(rows as i64)
    }

    pub fn cancel_active_by_loop_except(
        &self,
        loop_id: &str,
        keep_id: &str,
        finished_at: &str,
        reason: Option<&str>,
    ) -> Result<i64> {
        let rows = self.conn.execute(
            "UPDATE queue_items SET status='cancelled', finished_at=?3, last_error=?4, updated_at=?3 WHERE loop_id=?1 AND status IN ('queued','running') AND id != ?2",
            rusqlite::params![loop_id, keep_id, finished_at, reason],
        )?;
        Ok(rows as i64)
    }
}
