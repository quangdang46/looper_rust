-- Migration: 0008_archive_watermarks.sql
-- Purpose: Track per-source ingest watermarks for idempotent archive resume.

CREATE TABLE IF NOT EXISTS archive_watermarks (
    source_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    ingested_at TEXT NOT NULL,
    record_count INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(source_id, session_id),
    FOREIGN KEY(source_id) REFERENCES archive_sources(id)
);

CREATE INDEX IF NOT EXISTS idx_archive_watermarks_source
    ON archive_watermarks(source_id);

-- Add a unique constraint on archive_conversations to prevent double-insert
-- of the same session from the same source.
CREATE UNIQUE INDEX IF NOT EXISTS idx_archive_conversations_source_session
    ON archive_conversations(source_id, session_id);
