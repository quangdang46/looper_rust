use anyhow::{Context, Result};
use grove_types::{
    BeadId, BulletId, RunId,
    playbook::{
        BulletMaturity, BulletScope, BulletState, BulletType, FeedbackEventRecord, FeedbackKind,
        PlaybookBulletRecord,
    },
};
use rusqlite::{OptionalExtension, params};

use crate::{Database, timestamp_string};

impl Database {
    /// Insert a new playbook bullet.
    pub fn insert_playbook_bullet(&mut self, bullet: &PlaybookBulletRecord) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute(
                "INSERT INTO playbook_bullets(
                    id, scope, scope_key, category, text, bullet_type, state, maturity,
                    helpful_count, harmful_count, confidence_decay_half_life_days,
                    pinned, deprecated, replaced_by, deprecation_reason,
                    source_bead_ids_json, source_run_ids_json, tags_json,
                    effective_score, content_hash, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
                params![
                    bullet.id.as_str(),
                    scope_str(bullet.scope),
                    bullet.scope_key.as_deref(),
                    bullet.category,
                    bullet.text,
                    bullet_type_str(bullet.bullet_type),
                    state_str(bullet.state),
                    maturity_str(bullet.maturity),
                    bullet.helpful_count,
                    bullet.harmful_count,
                    bullet.confidence_decay_half_life_days,
                    bullet.pinned as i32,
                    bullet.deprecated as i32,
                    bullet.replaced_by.as_ref().map(|id: &BulletId| id.as_str()),
                    bullet.deprecation_reason.as_deref(),
                    serde_json::to_string(&bullet.source_bead_ids)?,
                    serde_json::to_string(&bullet.source_run_ids)?,
                    serde_json::to_string(&bullet.tags)?,
                    bullet.effective_score,
                    content_hash(&bullet.text),
                    timestamp_string(&bullet.created_at),
                    timestamp_string(&bullet.updated_at),
                ],
            )?;

            for feedback in &bullet.feedback_events {
                tx.execute(
                    "INSERT INTO playbook_feedback(bullet_id, kind, bead_id, run_id, context, weight, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        bullet.id.as_str(),
                        feedback_kind_str(feedback.kind),
                        feedback.bead_id.as_ref().map(|id: &BeadId| id.as_str()),
                        feedback.run_id.as_ref().map(|id: &RunId| id.as_str()),
                        feedback.context.as_deref(),
                        feedback.weight,
                        timestamp_string(&feedback.timestamp),
                    ],
                )?;
            }
            Ok(())
        })
        .with_context(|| format!("insert playbook bullet {}", bullet.id.as_str()))
    }

    /// Record a feedback event for a bullet and update counts.
    pub fn record_playbook_feedback(
        &mut self,
        bullet_id: &BulletId,
        feedback: &FeedbackEventRecord,
    ) -> Result<()> {
        self.with_tx(|tx| {
            tx.execute(
                "INSERT INTO playbook_feedback(bullet_id, kind, bead_id, run_id, context, weight, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    bullet_id.as_str(),
                    feedback_kind_str(feedback.kind),
                    feedback.bead_id.as_ref().map(|id: &BeadId| id.as_str()),
                    feedback.run_id.as_ref().map(|id: &RunId| id.as_str()),
                    feedback.context.as_deref(),
                    feedback.weight,
                    timestamp_string(&feedback.timestamp),
                ],
            )?;

            let count_update = match feedback.kind {
                FeedbackKind::Helpful => "helpful_count = helpful_count + 1",
                FeedbackKind::Harmful => "harmful_count = harmful_count + 1",
            };
            tx.execute(
                &format!(
                    "UPDATE playbook_bullets SET {count_update}, updated_at = ?1 WHERE id = ?2"
                ),
                params![timestamp_string(&feedback.timestamp), bullet_id.as_str()],
            )?;
            Ok(())
        })
        .with_context(|| format!("record feedback for bullet {}", bullet_id.as_str()))
    }

    /// List active playbook bullets, optionally filtered by scope.
    pub fn list_active_bullets(
        &self,
        scope: Option<BulletScope>,
    ) -> Result<Vec<PlaybookBulletRecord>> {
        self.list_playbook_bullets("state = 'active'", scope)
    }

    /// List all non-retired playbook bullets, optionally filtered by scope.
    pub fn list_non_retired_playbook_bullets(
        &self,
        scope: Option<BulletScope>,
    ) -> Result<Vec<PlaybookBulletRecord>> {
        self.list_playbook_bullets("state != 'retired'", scope)
    }

    fn list_playbook_bullets(
        &self,
        state_filter: &str,
        scope: Option<BulletScope>,
    ) -> Result<Vec<PlaybookBulletRecord>> {
        let sql = match scope {
            Some(_) => format!(
                "SELECT id, scope, scope_key, category, text, bullet_type, state, maturity,
                    helpful_count, harmful_count, confidence_decay_half_life_days,
                    pinned, deprecated, replaced_by, deprecation_reason,
                    source_bead_ids_json, source_run_ids_json, tags_json,
                    effective_score, created_at, updated_at
                 FROM playbook_bullets
                 WHERE {state_filter} AND scope = ?1
                 ORDER BY effective_score DESC NULLS LAST, created_at ASC"
            ),
            None => format!(
                "SELECT id, scope, scope_key, category, text, bullet_type, state, maturity,
                    helpful_count, harmful_count, confidence_decay_half_life_days,
                    pinned, deprecated, replaced_by, deprecation_reason,
                    source_bead_ids_json, source_run_ids_json, tags_json,
                    effective_score, created_at, updated_at
                 FROM playbook_bullets
                 WHERE {state_filter}
                 ORDER BY effective_score DESC NULLS LAST, created_at ASC"
            ),
        };

        let mut stmt = self.connection().prepare(&sql)?;
        let rows = match scope {
            Some(s) => stmt.query_map(params![scope_str(s)], map_bullet_row)?,
            None => stmt.query_map([], map_bullet_row)?,
        };

        let mut bullets = Vec::new();
        for row in rows {
            let mut bullet = row?;
            bullet.feedback_events = self.load_feedback_events(&bullet.id)?;
            bullets.push(bullet);
        }
        Ok(bullets)
    }

    fn load_feedback_events(&self, bullet_id: &BulletId) -> Result<Vec<FeedbackEventRecord>> {
        let mut stmt = self.connection().prepare(
            "SELECT kind, bead_id, run_id, context, weight, created_at
             FROM playbook_feedback
             WHERE bullet_id = ?1
             ORDER BY created_at ASC, id ASC",
        )?;

        let rows = stmt.query_map(params![bullet_id.as_str()], |row| {
            let kind: String = row.get(0)?;
            let bead_id: Option<String> = row.get(1)?;
            let run_id: Option<String> = row.get(2)?;
            let context: Option<String> = row.get(3)?;
            let weight: f32 = row.get(4)?;
            let created_at: String = row.get(5)?;
            Ok(FeedbackEventRecord {
                kind: parse_feedback_kind(&kind),
                timestamp: created_at.parse().unwrap_or_else(|_| chrono::Utc::now()),
                bead_id: bead_id.map(BeadId::new),
                run_id: run_id.map(RunId::new),
                context,
                weight,
            })
        })?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    /// Get a single bullet by ID.
    pub fn get_playbook_bullet(&self, bullet_id: &BulletId) -> Result<Option<PlaybookBulletRecord>> {
        let mut stmt = self.connection().prepare(
            "SELECT id, scope, scope_key, category, text, bullet_type, state, maturity,
                helpful_count, harmful_count, confidence_decay_half_life_days,
                pinned, deprecated, replaced_by, deprecation_reason,
                source_bead_ids_json, source_run_ids_json, tags_json,
                effective_score, created_at, updated_at
             FROM playbook_bullets
             WHERE id = ?1
             LIMIT 1",
        )?;
        let bullet = stmt
            .query_row(params![bullet_id.as_str()], map_bullet_row)
            .optional()?;

        match bullet {
            Some(mut bullet) => {
                bullet.feedback_events = self.load_feedback_events(&bullet.id)?;
                Ok(Some(bullet))
            }
            None => Ok(None),
        }
    }

    /// Update state and maturity for a bullet (promotion/demotion).
    pub fn update_bullet_maturity(
        &mut self,
        bullet_id: &BulletId,
        new_state: BulletState,
        new_maturity: BulletMaturity,
        effective_score: Option<f32>,
    ) -> Result<()> {
        let now = timestamp_string(&chrono::Utc::now());
        self.connection().execute(
            "UPDATE playbook_bullets SET state = ?1, maturity = ?2, effective_score = ?3, updated_at = ?4 WHERE id = ?5",
            params![
                state_str(new_state),
                maturity_str(new_maturity),
                effective_score,
                now,
                bullet_id.as_str(),
            ],
        ).with_context(|| format!("update bullet maturity {}", bullet_id.as_str()))?;
        Ok(())
    }

    /// Deprecate a bullet with reason.
    pub fn deprecate_bullet(
        &mut self,
        bullet_id: &BulletId,
        reason: &str,
        replaced_by: Option<&BulletId>,
    ) -> Result<()> {
        let now = timestamp_string(&chrono::Utc::now());
        self.connection().execute(
            "UPDATE playbook_bullets SET state = 'retired', maturity = 'deprecated', deprecated = 1,
             deprecation_reason = ?1, replaced_by = ?2, updated_at = ?3 WHERE id = ?4",
            params![
                reason,
                replaced_by.map(|id| id.as_str()),
                now,
                bullet_id.as_str(),
            ],
        ).with_context(|| format!("deprecate bullet {}", bullet_id.as_str()))?;
        Ok(())
    }

    /// Log a curation action.
    pub fn log_curation_action(
        &mut self,
        bullet_id: &BulletId,
        action: &str,
        reason: Option<&str>,
        old_state: Option<&str>,
        new_state: Option<&str>,
        old_maturity: Option<&str>,
        new_maturity: Option<&str>,
    ) -> Result<()> {
        let now = timestamp_string(&chrono::Utc::now());
        self.connection().execute(
            "INSERT INTO playbook_curation_log(bullet_id, action, reason, old_state, new_state, old_maturity, new_maturity, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                bullet_id.as_str(),
                action,
                reason,
                old_state,
                new_state,
                old_maturity,
                new_maturity,
                now,
            ],
        ).with_context(|| "insert curation log")?;
        Ok(())
    }

    /// Check if a bullet with the given content hash already exists.
    pub fn find_bullet_by_hash(&self, hash: &str) -> Result<Option<BulletId>> {
        let result: Option<String> = self.connection().query_row(
            "SELECT id FROM playbook_bullets WHERE content_hash = ?1 AND state != 'retired' LIMIT 1",
            params![hash],
            |row| row.get(0),
        ).optional()?;
        Ok(result.map(BulletId::new))
    }

    /// List all non-retired bullets (ID, text) for approximate deduplication in memory.
    pub fn list_non_retired_bullets(&self) -> Result<Vec<(BulletId, String)>> {
        let mut stmt = self
            .connection()
            .prepare("SELECT id, text FROM playbook_bullets WHERE state != 'retired'")?;

        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<(BulletId, String)> {
            let id: String = row.get(0)?;
            let text: String = row.get(1)?;
            Ok((BulletId::new(id), text))
        };

        let mut results = Vec::new();
        for row in stmt.query_map([], map_row)? {
            results.push(row?);
        }
        Ok(results)
    }
}

fn map_bullet_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlaybookBulletRecord> {
    let id_str: String = row.get(0)?;
    let scope_str: String = row.get(1)?;
    let scope_key: Option<String> = row.get(2)?;
    let category: String = row.get(3)?;
    let text: String = row.get(4)?;
    let bt_str: String = row.get(5)?;
    let state_s: String = row.get(6)?;
    let mat_str: String = row.get(7)?;
    let helpful_count: u32 = row.get(8)?;
    let harmful_count: u32 = row.get(9)?;
    let half_life: u32 = row.get(10)?;
    let pinned: i32 = row.get(11)?;
    let deprecated: i32 = row.get(12)?;
    let replaced_by: Option<String> = row.get(13)?;
    let deprecation_reason: Option<String> = row.get(14)?;
    let bead_ids_json: String = row.get(15)?;
    let run_ids_json: String = row.get(16)?;
    let tags_json: String = row.get(17)?;
    let effective_score: Option<f32> = row.get(18)?;
    let created_at: String = row.get(19)?;
    let updated_at: String = row.get(20)?;

    Ok(PlaybookBulletRecord {
        id: BulletId::new(id_str),
        scope: parse_scope(&scope_str),
        scope_key,
        category,
        text,
        bullet_type: parse_bullet_type(&bt_str),
        state: parse_state(&state_s),
        maturity: parse_maturity(&mat_str),
        helpful_count,
        harmful_count,
        feedback_events: Vec::new(),
        confidence_decay_half_life_days: half_life,
        pinned: pinned != 0,
        deprecated: deprecated != 0,
        replaced_by: replaced_by.map(BulletId::new),
        deprecation_reason,
        source_bead_ids: serde_json::from_str(&bead_ids_json).unwrap_or_default(),
        source_run_ids: serde_json::from_str(&run_ids_json).unwrap_or_default(),
        tags: serde_json::from_str(&tags_json).unwrap_or_default(),
        effective_score,
        created_at: created_at.parse().unwrap_or_else(|_| chrono::Utc::now()),
        updated_at: updated_at.parse().unwrap_or_else(|_| chrono::Utc::now()),
    })
}

// Helpers

fn content_hash(text: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let normalized = text.trim().to_lowercase();
    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn scope_str(scope: BulletScope) -> &'static str {
    match scope {
        BulletScope::Global => "global",
        BulletScope::Workspace => "workspace",
        BulletScope::Language => "language",
        BulletScope::Framework => "framework",
        BulletScope::Bead => "bead",
    }
}

fn bullet_type_str(bt: BulletType) -> &'static str {
    match bt {
        BulletType::Rule => "rule",
        BulletType::AntiPattern => "anti_pattern",
    }
}

fn state_str(state: BulletState) -> &'static str {
    match state {
        BulletState::Draft => "draft",
        BulletState::Active => "active",
        BulletState::Retired => "retired",
    }
}

fn maturity_str(maturity: BulletMaturity) -> &'static str {
    match maturity {
        BulletMaturity::Candidate => "candidate",
        BulletMaturity::Established => "established",
        BulletMaturity::Proven => "proven",
        BulletMaturity::Deprecated => "deprecated",
    }
}

fn feedback_kind_str(kind: FeedbackKind) -> &'static str {
    match kind {
        FeedbackKind::Helpful => "helpful",
        FeedbackKind::Harmful => "harmful",
    }
}

fn parse_feedback_kind(s: &str) -> FeedbackKind {
    match s {
        "harmful" => FeedbackKind::Harmful,
        _ => FeedbackKind::Helpful,
    }
}

fn parse_scope(s: &str) -> BulletScope {
    match s {
        "workspace" => BulletScope::Workspace,
        "language" => BulletScope::Language,
        "framework" => BulletScope::Framework,
        "bead" => BulletScope::Bead,
        _ => BulletScope::Global,
    }
}

fn parse_bullet_type(s: &str) -> BulletType {
    match s {
        "anti_pattern" => BulletType::AntiPattern,
        _ => BulletType::Rule,
    }
}

fn parse_state(s: &str) -> BulletState {
    match s {
        "active" => BulletState::Active,
        "retired" => BulletState::Retired,
        _ => BulletState::Draft,
    }
}

fn parse_maturity(s: &str) -> BulletMaturity {
    match s {
        "established" => BulletMaturity::Established,
        "proven" => BulletMaturity::Proven,
        "deprecated" => BulletMaturity::Deprecated,
        _ => BulletMaturity::Candidate,
    }
}
