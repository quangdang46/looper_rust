-- Migration: 0004_mirror_outbox.sql
-- Purpose: Add mirror outbox table for durable br sync retries
--
-- This migration implements grove-1j9.7.6: mirror outbox for durable `br` sync retries.
-- It preserves successful local completion even when the follow-up mirror back to `br`
-- temporarily fails, and makes the resulting local-success / mirror-pending /
-- mirror-failed states explicit to operators.

-- Mirror outbox: tracks beads that succeeded locally but haven't been mirrored to br yet
CREATE TABLE IF NOT EXISTS mirror_outbox (
    -- Primary key
    id TEXT PRIMARY KEY NOT NULL,

    -- Bead identification
    bead_id TEXT NOT NULL,

    -- The run that produced the result to mirror
    run_id TEXT NOT NULL,

    -- The handoff result to mirror (contains summary, artifacts, lessons, decisions, warnings)
    handoff_json TEXT NOT NULL,

    -- Close request: when set, we'll attempt to close the bead in br
    close_bead INTEGER NOT NULL DEFAULT 1,

    -- Mirror state tracking
    mirror_status TEXT NOT NULL DEFAULT 'pending', -- pending, in_progress, succeeded, failed

    -- Retry tracking
    attempt_count INTEGER NOT NULL DEFAULT 0,
    last_attempt_at TEXT,
    next_retry_after TEXT,
    last_error TEXT,

    -- Timestamps
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,

    -- Foreign key relationships (informal - SQLite doesn't enforce these)
    FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id),
    FOREIGN KEY (run_id) REFERENCES task_runs(id)
);

-- Index for finding pending mirror operations
CREATE INDEX IF NOT EXISTS idx_mirror_outbox_status
    ON mirror_outbox(mirror_status, next_retry_after)
    WHERE mirror_status IN ('pending', 'failed');

-- Index for looking up by bead
CREATE INDEX IF NOT EXISTS idx_mirror_outbox_bead
    ON mirror_outbox(bead_id);

-- Index for looking up by run
CREATE INDEX IF NOT EXISTS idx_mirror_outbox_run
    ON mirror_outbox(run_id);
