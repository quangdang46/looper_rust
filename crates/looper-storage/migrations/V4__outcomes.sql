-- Outcome learning: per-execution result tracking with trend queries
-- Inspired by ContribAI's per-repo outcome SQLite with TTL aging

CREATE TABLE IF NOT EXISTS outcomes (
    id TEXT PRIMARY KEY,
    loop_id TEXT,
    run_id TEXT,
    project_id TEXT NOT NULL,
    repo TEXT,
    loop_type TEXT NOT NULL,
    status TEXT NOT NULL,       -- success, failed, timeout, cancelled
    duration_ms INTEGER,
    exit_code INTEGER,
    output_hash TEXT,           -- sha256 of output for dedup
    error_message TEXT,
    error_kind TEXT,
    metadata_json TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S.000Z', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S.000Z', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_outcomes_project_id ON outcomes(project_id);
CREATE INDEX IF NOT EXISTS idx_outcomes_loop_id ON outcomes(loop_id);
CREATE INDEX IF NOT EXISTS idx_outcomes_created_at ON outcomes(created_at);
CREATE INDEX IF NOT EXISTS idx_outcomes_trend ON outcomes(project_id, loop_type, status, created_at);
CREATE INDEX IF NOT EXISTS idx_outcomes_output_hash ON outcomes(output_hash);
