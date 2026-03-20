-- Migration: 0009_playbook.sql
-- Purpose: Playbook data model for evidence-scored bullets, feedback events, and maturity state.

CREATE TABLE IF NOT EXISTS playbook_bullets (
    id TEXT PRIMARY KEY,
    scope TEXT NOT NULL DEFAULT 'global',        -- global, workspace, language, framework, bead
    scope_key TEXT,                               -- e.g. workspace name or bead_id
    category TEXT NOT NULL DEFAULT 'general',
    text TEXT NOT NULL,
    bullet_type TEXT NOT NULL DEFAULT 'rule',     -- rule, anti_pattern
    state TEXT NOT NULL DEFAULT 'draft',          -- draft, active, retired
    maturity TEXT NOT NULL DEFAULT 'candidate',   -- candidate, established, proven, deprecated
    helpful_count INTEGER NOT NULL DEFAULT 0,
    harmful_count INTEGER NOT NULL DEFAULT 0,
    confidence_decay_half_life_days INTEGER NOT NULL DEFAULT 30,
    pinned INTEGER NOT NULL DEFAULT 0,
    deprecated INTEGER NOT NULL DEFAULT 0,
    replaced_by TEXT,
    deprecation_reason TEXT,
    source_bead_ids_json TEXT NOT NULL DEFAULT '[]',
    source_run_ids_json TEXT NOT NULL DEFAULT '[]',
    tags_json TEXT NOT NULL DEFAULT '[]',
    effective_score REAL,
    content_hash TEXT NOT NULL,                    -- SHA-256 of normalized text for dedup
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(replaced_by) REFERENCES playbook_bullets(id)
);

CREATE INDEX IF NOT EXISTS idx_playbook_bullets_state
    ON playbook_bullets(state);
CREATE INDEX IF NOT EXISTS idx_playbook_bullets_maturity
    ON playbook_bullets(maturity);
CREATE INDEX IF NOT EXISTS idx_playbook_bullets_scope
    ON playbook_bullets(scope, scope_key);
CREATE INDEX IF NOT EXISTS idx_playbook_bullets_hash
    ON playbook_bullets(content_hash);

CREATE TABLE IF NOT EXISTS playbook_feedback (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    bullet_id TEXT NOT NULL,
    kind TEXT NOT NULL,                -- helpful, harmful
    bead_id TEXT,
    run_id TEXT,
    context TEXT,
    weight REAL NOT NULL DEFAULT 1.0,
    created_at TEXT NOT NULL,
    FOREIGN KEY(bullet_id) REFERENCES playbook_bullets(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_playbook_feedback_bullet
    ON playbook_feedback(bullet_id);
CREATE INDEX IF NOT EXISTS idx_playbook_feedback_kind
    ON playbook_feedback(kind);

-- Playbook curation log for auditability
CREATE TABLE IF NOT EXISTS playbook_curation_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    bullet_id TEXT NOT NULL,
    action TEXT NOT NULL,               -- add, helpful, harmful, replace, deprecate, merge, promote, demote
    reason TEXT,
    old_state TEXT,
    new_state TEXT,
    old_maturity TEXT,
    new_maturity TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_playbook_curation_log_bullet
    ON playbook_curation_log(bullet_id);
