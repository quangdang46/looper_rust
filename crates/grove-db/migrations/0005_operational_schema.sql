-- Migration: 0005_operational_schema.sql
-- Purpose: Add operational schema addendum for prompt provenance, dispatch decisions, config snapshots, and integrity checks
--
-- This migration implements the rest of grove-1j9.7.9 (leader leases were added in 0003).

-- 21.2 Prompt materialization table
CREATE TABLE IF NOT EXISTS prompt_materializations (
  id TEXT PRIMARY KEY,
  bead_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  prompt_path TEXT NOT NULL,
  prompt_hash TEXT NOT NULL,
  byte_count INTEGER NOT NULL,
  segment_manifest_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE,
  FOREIGN KEY (run_id) REFERENCES task_runs(id) ON DELETE CASCADE,
  FOREIGN KEY (session_id) REFERENCES claude_sessions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_prompt_materializations_bead_created 
  ON prompt_materializations(bead_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_prompt_materializations_run_created 
  ON prompt_materializations(run_id, created_at DESC);

-- 21.3 Dispatch decision table
CREATE TABLE IF NOT EXISTS dispatch_decisions (
  id TEXT PRIMARY KEY,
  bead_id TEXT NOT NULL,
  tick_id TEXT NOT NULL,
  disposition TEXT NOT NULL,
  score_breakdown_json TEXT NOT NULL,
  blocking_reasons_json TEXT NOT NULL DEFAULT '[]',
  competing_bead_ids_json TEXT NOT NULL DEFAULT '[]',
  created_at TEXT NOT NULL,
  FOREIGN KEY (bead_id) REFERENCES bead_cache(bead_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_dispatch_decisions_bead_created 
  ON dispatch_decisions(bead_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_dispatch_decisions_tick 
  ON dispatch_decisions(tick_id);

-- 21.5 Config snapshots table
CREATE TABLE IF NOT EXISTS config_snapshots (
  id TEXT PRIMARY KEY,
  sha256 TEXT NOT NULL,
  source_path TEXT NOT NULL,
  config_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_config_snapshots_sha256 
  ON config_snapshots(sha256);

-- 21.6 Integrity checks table
CREATE TABLE IF NOT EXISTS integrity_checks (
  id TEXT PRIMARY KEY,
  scope TEXT NOT NULL,
  scope_key TEXT,
  status TEXT NOT NULL,
  findings_json TEXT NOT NULL DEFAULT '[]',
  created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_integrity_checks_scope_created 
  ON integrity_checks(scope, scope_key, created_at DESC);
