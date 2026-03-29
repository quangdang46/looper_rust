-- Migration: 0014_cleanup_snapshots.sql
-- Purpose: Persist durable cleanup summaries before removing raw workspace artifacts.

CREATE TABLE IF NOT EXISTS cleanup_snapshots (
    id TEXT PRIMARY KEY,
    bead_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    cleaned_artifact_paths_json TEXT NOT NULL,
    cleaned_artifact_kinds_json TEXT NOT NULL,
    deleted_bytes INTEGER NOT NULL DEFAULT 0,
    continuity_summary TEXT NOT NULL,
    next_bead_guidance TEXT NOT NULL,
    lessons_json TEXT NOT NULL,
    decisions_json TEXT NOT NULL,
    warnings_json TEXT NOT NULL,
    prompt_summary TEXT NOT NULL,
    transcript_tail_summary TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE,
    FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (session_id) REFERENCES claude_sessions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_cleanup_snapshots_bead_created
ON cleanup_snapshots(bead_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_cleanup_snapshots_session
ON cleanup_snapshots(session_id, created_at DESC);
