-- Takeover sessions: single-PR focus mode
CREATE TABLE IF NOT EXISTS takeover_sessions (
    id TEXT PRIMARY KEY,
    project_name TEXT NOT NULL,
    repo_owner TEXT NOT NULL,
    repo_name TEXT NOT NULL,
    pr_number INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    started_at TEXT NOT NULL,
    last_activity TEXT NOT NULL,
    cycles_completed INTEGER NOT NULL DEFAULT 0,
    current_phase TEXT NOT NULL DEFAULT 'planning',
    error_count INTEGER NOT NULL DEFAULT 0,
    max_errors INTEGER NOT NULL DEFAULT 5,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_takeover_sessions_status ON takeover_sessions(status);
CREATE INDEX IF NOT EXISTS idx_takeover_sessions_project ON takeover_sessions(project_name);
