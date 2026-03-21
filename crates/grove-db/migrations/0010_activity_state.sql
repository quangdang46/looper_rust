-- Migration: 0010_activity_state.sql
-- Purpose: Persist per-run agent activity state and escalation tier for autonomous recovery analysis

ALTER TABLE task_runs ADD COLUMN activity TEXT;
ALTER TABLE task_runs ADD COLUMN last_activity_at TEXT;
ALTER TABLE task_runs ADD COLUMN escalation_tier TEXT NOT NULL DEFAULT 'FirstAttempt';

CREATE INDEX IF NOT EXISTS idx_task_runs_activity ON task_runs(activity);
CREATE INDEX IF NOT EXISTS idx_task_runs_last_activity_at ON task_runs(last_activity_at);
CREATE INDEX IF NOT EXISTS idx_task_runs_escalation_tier ON task_runs(escalation_tier);
