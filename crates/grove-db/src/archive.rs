use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Row};
use grove_types::{
    BeadId, RunId, SessionId, SourceId,
    archive::{
        ConversationRecord, MessageRecord, MessageRole, RelevantSnippet, RetrievalBundle,
        SnippetRecord, SourceRecord,
    },
};

use crate::{Database, timestamp_string, parse_json};

impl Database {
    pub fn insert_source_record(&mut self, record: &SourceRecord) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO archive_sources(id, source_path, origin_host, metadata_json) 
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    record.id.as_str(),
                    record.source_path.as_str(),
                    record.origin_host.as_deref(),
                    serde_json::to_string(&record.metadata_json)?,
                ],
            )?;
            Ok(())
        })
        .with_context(|| format!("insert source record {}", record.id.as_str()))
    }

    pub fn insert_conversation_record(&mut self, record: &ConversationRecord) -> Result<i64> {
        self.with_tx(|tx| {
            tx.execute(
                "INSERT INTO archive_conversations(
                    bead_id, run_id, session_id, workspace, title, source_path,
                    started_at, ended_at, approx_tokens, metadata_json, source_id, origin_host
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    record.bead_id.as_ref().map(|id: &BeadId| id.as_str()),
                    record.run_id.as_ref().map(|id: &RunId| id.as_str()),
                    record.session_id.as_str(),
                    record.workspace.as_ref().map(|w: &camino::Utf8PathBuf| w.as_str()),
                    record.title.as_deref(),
                    record.source_path.as_str(),
                    record.started_at.as_ref().map(timestamp_string),
                    record.ended_at.as_ref().map(timestamp_string),
                    record.approx_tokens,
                    serde_json::to_string(&record.metadata_json)?,
                    record.source_id.as_str(),
                    record.origin_host.as_deref(),
                ],
            )?;
            let conversation_id = tx.last_insert_rowid();

            for message in &record.messages {
                tx.execute(
                    "INSERT INTO archive_messages(
                        conversation_id, idx, role, author, created_at, content, extra_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        conversation_id,
                        message.idx,
                        match message.role {
                            MessageRole::User => "user",
                            MessageRole::Agent => "agent",
                            MessageRole::Tool => "tool",
                            MessageRole::System => "system",
                            MessageRole::Other(ref s) => s.as_str(),
                        },
                        message.author.as_deref(),
                        message.created_at.as_ref().map(timestamp_string),
                        message.content,
                        serde_json::to_string(&message.extra_json)?,
                    ],
                )?;
                let message_id = tx.last_insert_rowid();

                for snippet in &message.snippets {
                    tx.execute(
                        "INSERT INTO archive_snippets(
                            message_id, file_path, start_line, end_line, language, snippet_text
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            message_id,
                            snippet
                                .file_path
                                .as_ref()
                                .map(|p: &camino::Utf8PathBuf| p.as_str()),
                            snippet.start_line,
                            snippet.end_line,
                            snippet.language.as_deref(),
                            snippet.snippet_text.as_deref(),
                        ],
                    )?;
                }
            }
            Ok(conversation_id)
        })
        .with_context(|| format!("insert conversation {}", record.session_id.as_str()))
    }

    /// Primary lexical search powered by FTS5
    pub fn search_archive_fts(&self, query_text: &str, limit: usize) -> Result<RetrievalBundle> {
        let mut stmt = self.connection().prepare(
            "SELECT
                m.id AS message_id,
                m.conversation_id,
                c.source_path,
                snippet(archive_fts, 3, '<b>', '</b>', '...', 16) AS highlighted,
                bm25(archive_fts) AS score
             FROM archive_fts fts
             JOIN archive_messages m ON fts.rowid = m.id
             JOIN archive_conversations c ON m.conversation_id = c.id
             WHERE archive_fts MATCH ?1
             ORDER BY score ASC
             LIMIT ?2"
        )?;

        let mut snippets = Vec::new();
        let mut conversations = std::collections::HashSet::new();

        let rows = stmt.query_map(params![query_text, limit as i64], |row| {
            let conversation_id: i64 = row.get(1)?;
            let score: f64 = row.get(4)?;
            // BM25 is usually negative in SQLite FTS5 (more negative = better)
            let score_f32 = -score as f32;

            Ok(RelevantSnippet {
                message_id: row.get(0)?,
                conversation_id,
                file_path: row.get::<_, Option<String>>(2)?.map(camino::Utf8PathBuf::from),
                snippet: row.get(3)?,
                score: score_f32,
            })
        })?;

        for row in rows {
            let snippet = row?;
            conversations.insert(snippet.conversation_id);
            snippets.push(snippet);
        }

        Ok(RetrievalBundle {
            snippets,
            conversations: conversations.into_iter().collect(),
        })
    }

    /// Check if a session has already been ingested for a given source.
    pub fn is_session_ingested(&self, source_id: &str, session_id: &str) -> Result<bool> {
        let count: i64 = self.connection().query_row(
            "SELECT COUNT(*) FROM archive_watermarks WHERE source_id = ?1 AND session_id = ?2",
            params![source_id, session_id],
            |row| row.get(0),
        ).context("check archive watermark")?;
        Ok(count > 0)
    }

    /// Record a watermark after successful ingest.
    pub fn record_ingest_watermark(
        &mut self,
        source_id: &str,
        session_id: &str,
        record_count: i64,
    ) -> Result<()> {
        let now = timestamp_string(&chrono::Utc::now());
        self.connection().execute(
            "INSERT OR REPLACE INTO archive_watermarks(source_id, session_id, ingested_at, record_count)
             VALUES (?1, ?2, ?3, ?4)",
            params![source_id, session_id, now, record_count],
        ).context("record ingest watermark")?;
        Ok(())
    }

    /// Idempotent conversation insert — skips if already archived for this source+session.
    pub fn insert_conversation_idempotent(&mut self, record: &ConversationRecord) -> Result<Option<i64>> {
        if self.is_session_ingested(record.source_id.as_str(), record.session_id.as_str())? {
            return Ok(None);
        }

        let conversation_id = self.insert_conversation_record(record)?;

        let message_count = record.messages.len() as i64;
        self.record_ingest_watermark(
            record.source_id.as_str(),
            record.session_id.as_str(),
            message_count,
        )?;

        Ok(Some(conversation_id))
    }

    /// List all watermarks for a given source.
    pub fn list_watermarks_for_source(&self, source_id: &str) -> Result<Vec<(String, String, i64)>> {
        let mut stmt = self.connection().prepare(
            "SELECT session_id, ingested_at, record_count FROM archive_watermarks WHERE source_id = ?1 ORDER BY ingested_at DESC"
        )?;
        let rows = stmt.query_map(params![source_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }
}

