//! Playbook scoring engine — exponential decay scoring, maturity promotion/demotion,
//! and validation gate for promoting playbook rules.
//!
//! Implements the scoring algorithm from PLAN.md §1.2.2-1.2.3.

use anyhow::Result;
use grove_db::Database;
use grove_types::{
    BulletId,
    playbook::{
        BulletMaturity, BulletScope, BulletState, BulletType, FeedbackKind, PlaybookBulletRecord,
    },
};

/// Scoring configuration (matches PLAN.md defaults).
#[derive(Debug, Clone)]
pub struct ScoringConfig {
    /// Half-life in days for exponential decay (default: 30).
    pub half_life_days: f64,
    /// Multiplier for harmful feedback weight (default: 4.0).
    pub harmful_multiplier: f64,
    /// Score threshold to promote from Candidate to Established (default: 2.0).
    pub promote_threshold: f64,
    /// Score threshold to promote from Established to Proven (default: 5.0).
    pub proven_threshold: f64,
    /// Harmful ratio threshold for auto-deprecation (default: 0.3).
    pub harmful_ratio_threshold: f64,
    /// Negative score threshold for auto-prune (default: -3.0).
    pub prune_threshold: f64,
    /// Minimum feedback events before promotion is considered.
    pub min_events_for_promotion: u32,
    /// Days after which a bullet with no feedback is considered stale.
    pub staleness_days: u32,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            half_life_days: 30.0,
            harmful_multiplier: 4.0,
            promote_threshold: 2.0,
            proven_threshold: 5.0,
            harmful_ratio_threshold: 0.3,
            prune_threshold: -3.0,
            min_events_for_promotion: 3,
            staleness_days: 90,
        }
    }
}

/// Compute the effective score for a bullet using exponential decay.
pub fn effective_score(bullet: &PlaybookBulletRecord, config: &ScoringConfig) -> f64 {
    let now = chrono::Utc::now();

    let decayed_helpful: f64 = bullet
        .feedback_events
        .iter()
        .filter(|f| f.kind == FeedbackKind::Helpful)
        .map(|f| {
            let age = (now - f.timestamp).num_days().max(0) as f64;
            f.weight as f64 * 0.5_f64.powf(age / config.half_life_days)
        })
        .sum();

    let decayed_harmful: f64 = bullet
        .feedback_events
        .iter()
        .filter(|f| f.kind == FeedbackKind::Harmful)
        .map(|f| {
            let age = (now - f.timestamp).num_days().max(0) as f64;
            f.weight as f64 * 0.5_f64.powf(age / config.half_life_days)
        })
        .sum();

    let maturity_scale = match bullet.maturity {
        BulletMaturity::Candidate => 0.5,
        BulletMaturity::Established => 1.0,
        BulletMaturity::Proven => 1.5,
        BulletMaturity::Deprecated => 0.0,
    };

    (decayed_helpful - config.harmful_multiplier * decayed_harmful) * maturity_scale
}

/// Compute the harmful ratio for a bullet.
pub fn harmful_ratio(bullet: &PlaybookBulletRecord, config: &ScoringConfig) -> f64 {
    let now = chrono::Utc::now();

    let decayed_helpful: f64 = bullet
        .feedback_events
        .iter()
        .filter(|f| f.kind == FeedbackKind::Helpful)
        .map(|f| {
            let age = (now - f.timestamp).num_days().max(0) as f64;
            f.weight as f64 * 0.5_f64.powf(age / config.half_life_days)
        })
        .sum();

    let decayed_harmful: f64 = bullet
        .feedback_events
        .iter()
        .filter(|f| f.kind == FeedbackKind::Harmful)
        .map(|f| {
            let age = (now - f.timestamp).num_days().max(0) as f64;
            f.weight as f64 * 0.5_f64.powf(age / config.half_life_days)
        })
        .sum();

    let total = decayed_helpful + decayed_harmful;
    if total == 0.0 {
        0.0
    } else {
        decayed_harmful / total
    }
}

/// Determine the target maturity based on the current score and event count.
pub fn target_maturity(
    bullet: &PlaybookBulletRecord,
    score: f64,
    config: &ScoringConfig,
) -> BulletMaturity {
    let total_events = bullet.helpful_count + bullet.harmful_count;
    let ratio = harmful_ratio(bullet, config);

    // Auto-deprecate if harmful ratio exceeds threshold
    if ratio > config.harmful_ratio_threshold {
        return BulletMaturity::Deprecated;
    }

    // Auto-prune if score is deeply negative
    if score < config.prune_threshold {
        return BulletMaturity::Deprecated;
    }

    // Auto-prune stale drafts that never got traction
    if bullet.state == BulletState::Draft {
        let now = chrono::Utc::now();
        let age_days = (now - bullet.created_at).num_days().max(0) as u32;
        if age_days > config.staleness_days && total_events < config.min_events_for_promotion {
            return BulletMaturity::Deprecated;
        }
    }

    // Promotion gates
    if total_events < config.min_events_for_promotion {
        return bullet.maturity; // not enough evidence yet
    }

    match bullet.maturity {
        BulletMaturity::Candidate if score > config.promote_threshold => {
            BulletMaturity::Established
        }
        BulletMaturity::Established if score > config.proven_threshold => {
            BulletMaturity::Proven
        }
        // Demotion: if score drops below zero, demote one level
        BulletMaturity::Proven if score < 0.0 => BulletMaturity::Established,
        BulletMaturity::Established if score < 0.0 => BulletMaturity::Candidate,
        other => other,
    }
}

/// Run the scoring and promotion/demotion pass across all non-retired bullets.
///
/// Returns the number of bullets that changed maturity.
pub fn run_scoring_pass(db: &mut Database, config: &ScoringConfig) -> Result<usize> {
    let bullets = db.list_non_retired_playbook_bullets(None)?;
    let mut changed = 0;

    for bullet in &bullets {
        let score = effective_score(bullet, config);
        let new_maturity = target_maturity(bullet, score, config);

        let new_state = if new_maturity == BulletMaturity::Deprecated {
            BulletState::Retired
        } else if bullet.state == BulletState::Draft
            && new_maturity >= BulletMaturity::Established
        {
            BulletState::Active
        } else {
            bullet.state
        };

        if new_maturity != bullet.maturity || new_state != bullet.state {
            if new_maturity == BulletMaturity::Deprecated {
                let mut replaced_by: Option<BulletId> = None;

                // Invert Rule into an AntiPattern if we have enough statistical evidence
                // that this rule is systematically harmful.
                let total_events = bullet.helpful_count + bullet.harmful_count;
                if bullet.bullet_type == BulletType::Rule && total_events >= config.min_events_for_promotion {
                    let anti_text = format!("AVOID: {}", bullet.text);
                    let inverted_id = BulletId::new(format!("{}-inv", bullet.id.as_str()));

                    let anti_bullet = PlaybookBulletRecord {
                        id: inverted_id.clone(),
                        scope: bullet.scope,
                        scope_key: bullet.scope_key.clone(),
                        category: format!("{}_inverted", bullet.category),
                        text: anti_text,
                        bullet_type: BulletType::AntiPattern,
                        state: BulletState::Draft,
                        maturity: BulletMaturity::Candidate,
                        helpful_count: 0,
                        harmful_count: 0,
                        feedback_events: vec![],
                        confidence_decay_half_life_days: bullet.confidence_decay_half_life_days,
                        pinned: false,
                        deprecated: false,
                        replaced_by: None,
                        deprecation_reason: None,
                        source_bead_ids: bullet.source_bead_ids.clone(),
                        source_run_ids: bullet.source_run_ids.clone(),
                        tags: bullet.tags.clone(),
                        effective_score: Some(1.0), // give it a fresh start
                        created_at: chrono::Utc::now(),
                        updated_at: chrono::Utc::now(),
                    };

                    if db.insert_playbook_bullet(&anti_bullet).is_ok() {
                        replaced_by = Some(inverted_id);
                    }
                }

                db.deprecate_bullet(
                    &bullet.id,
                    "rule was inverted to anti-pattern due to consecutive harmful outcome ratios or negative score thresholds",
                    replaced_by.as_ref(), // Pass the reference
                )?;
            } else {
                db.update_bullet_maturity(
                    &bullet.id,
                    new_state,
                    new_maturity,
                    Some(score as f32),
                )?;
            }

            db.log_curation_action(
                &bullet.id,
                if new_maturity > bullet.maturity {
                    "promote"
                } else {
                    "demote"
                },
                Some(&format!("score={score:.2}")),
                Some(&format!("{:?}", bullet.state)),
                Some(&format!("{:?}", new_state)),
                Some(&format!("{:?}", bullet.maturity)),
                Some(&format!("{:?}", new_maturity)),
            )?;

            changed += 1;
        } else if (bullet.effective_score.unwrap_or(0.0) - score as f32).abs() > 0.01 {
            // Just update the score without changing maturity
            db.update_bullet_maturity(
                &bullet.id,
                bullet.state,
                bullet.maturity,
                Some(score as f32),
            )?;
        }
    }

    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grove_types::playbook::{
        BulletMaturity, BulletScope, BulletState, BulletType, FeedbackEventRecord, FeedbackKind,
    };
    use grove_types::{BeadId, BulletId, RunId};

    fn sample_bullet() -> PlaybookBulletRecord {
        PlaybookBulletRecord {
            id: BulletId::new("blt-test"),
            scope: BulletScope::Global,
            scope_key: None,
            category: "test".to_string(),
            text: "Always validate inputs before processing".to_string(),
            bullet_type: BulletType::Rule,
            state: BulletState::Active,
            maturity: BulletMaturity::Candidate,
            helpful_count: 3,
            harmful_count: 0,
            feedback_events: vec![
                FeedbackEventRecord {
                    kind: FeedbackKind::Helpful,
                    timestamp: chrono::Utc::now(),
                    bead_id: Some(BeadId::new("grove-1")),
                    run_id: Some(RunId::new("run-1")),
                    context: None,
                    weight: 1.0,
                },
                FeedbackEventRecord {
                    kind: FeedbackKind::Helpful,
                    timestamp: chrono::Utc::now(),
                    bead_id: Some(BeadId::new("grove-2")),
                    run_id: Some(RunId::new("run-2")),
                    context: None,
                    weight: 1.0,
                },
                FeedbackEventRecord {
                    kind: FeedbackKind::Helpful,
                    timestamp: chrono::Utc::now(),
                    bead_id: Some(BeadId::new("grove-3")),
                    run_id: Some(RunId::new("run-3")),
                    context: None,
                    weight: 1.0,
                },
            ],
            confidence_decay_half_life_days: 30,
            pinned: false,
            deprecated: false,
            replaced_by: None,
            deprecation_reason: None,
            source_bead_ids: vec![],
            source_run_ids: vec![],
            tags: vec![],
            effective_score: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn score_with_all_helpful_should_be_positive() {
        let bullet = sample_bullet();
        let config = ScoringConfig::default();
        let score = effective_score(&bullet, &config);
        assert!(score > 0.0, "score with 3 helpful events should be positive: {score}");
    }

    #[test]
    fn candidate_with_strong_evidence_should_promote_to_established() {
        let mut bullet = sample_bullet();
        bullet.helpful_count = 5;
        bullet.feedback_events.extend([
            FeedbackEventRecord {
                kind: FeedbackKind::Helpful,
                timestamp: chrono::Utc::now(),
                bead_id: Some(BeadId::new("grove-4")),
                run_id: Some(RunId::new("run-4")),
                context: None,
                weight: 1.0,
            },
            FeedbackEventRecord {
                kind: FeedbackKind::Helpful,
                timestamp: chrono::Utc::now(),
                bead_id: Some(BeadId::new("grove-5")),
                run_id: Some(RunId::new("run-5")),
                context: None,
                weight: 1.0,
            },
        ]);

        let config = ScoringConfig::default();
        let score = effective_score(&bullet, &config);
        let new_maturity = target_maturity(&bullet, score, &config);
        assert_eq!(
            new_maturity,
            BulletMaturity::Established,
            "candidate with strong recent helpful evidence should promote"
        );
    }

    #[test]
    fn high_harmful_ratio_triggers_deprecation() {
        let mut bullet = sample_bullet();
        bullet.harmful_count = 5;
        bullet.helpful_count = 1;
        bullet.feedback_events = vec![
            FeedbackEventRecord {
                kind: FeedbackKind::Harmful,
                timestamp: chrono::Utc::now(),
                bead_id: None,
                run_id: None,
                context: None,
                weight: 1.0,
            };
            5
        ];
        let config = ScoringConfig::default();
        let score = effective_score(&bullet, &config);
        let new_maturity = target_maturity(&bullet, score, &config);
        assert_eq!(new_maturity, BulletMaturity::Deprecated);
    }

    #[test]
    fn insufficient_events_prevents_promotion() {
        let mut bullet = sample_bullet();
        bullet.helpful_count = 1;
        bullet.harmful_count = 0;
        bullet.feedback_events = vec![FeedbackEventRecord {
            kind: FeedbackKind::Helpful,
            timestamp: chrono::Utc::now(),
            bead_id: None,
            run_id: None,
            context: None,
            weight: 5.0, // high weight but only 1 event
        }];
        let config = ScoringConfig::default();
        let score = effective_score(&bullet, &config);
        let new_maturity = target_maturity(&bullet, score, &config);
        assert_eq!(
            new_maturity,
            BulletMaturity::Candidate,
            "should not promote with insufficient events"
        );
    }
}
