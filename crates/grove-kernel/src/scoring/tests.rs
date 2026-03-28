
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
    assert!(
        score > 0.0,
        "score with 3 helpful events should be positive: {score}"
    );
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
