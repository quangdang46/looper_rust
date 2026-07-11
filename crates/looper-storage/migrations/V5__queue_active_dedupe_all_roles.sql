-- Expand one-active-dedupe uniqueness from reviewer/fixer only to all
-- role types used by admit-work and discovery (planner, reviewer, worker, fixer).
-- Required so create_or_get_active_by_dedupe is idempotent for planner/worker.

DROP INDEX IF EXISTS idx_queue_items_one_active_dedupe;

CREATE UNIQUE INDEX IF NOT EXISTS idx_queue_items_one_active_dedupe
    ON queue_items (dedupe_key)
    WHERE type IN ('planner', 'reviewer', 'worker', 'fixer')
      AND status IN ('queued', 'running');
