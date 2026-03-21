use assert_cmd::Command;
use chrono::Utc;
use grove_db::Database;
use grove_types::{
    BeadId, RunId, SessionOutcome, SessionStatus,
    playbook::{BulletMaturity, BulletState, BulletType, PlaybookBulletRecord, BulletScope},
    prompt::{PromptManifest, PromptManifestSection, PromptSegmentKind, PromptSectionProvenance},
};
use tempfile::TempDir;
use grove_kernel::{diary, inspect_view, lesson_ingest, scoring};
use grove_kernel::scoring::ScoringConfig;
use std::fs;
use grove_session::{PromptMaterializationInput, materialize_prompt};

// 1. Outcome-derived feedback creates events
#[test]
fn test_diary_implicit_feedback() {
    let temp = TempDir::new().unwrap();
    let db_path = temp.path().join("grove.db");
    let db_path = camino::Utf8PathBuf::from_path_buf(db_path).unwrap();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();

    let bead_id = BeadId::new("test-1");
    let run_id = RunId::new("run-1");

    // Insert a draft bullet
    let bullet_id = grove_types::BulletId::new("blt-1");
    let bullet = PlaybookBulletRecord {
        id: bullet_id.clone(),
        scope: BulletScope::Global,
        scope_key: None,
        category: "test".to_string(),
        text: "Make sure all code returns an explicit error type".to_string(),
        bullet_type: BulletType::Rule,
        state: BulletState::Draft,
        maturity: BulletMaturity::Candidate,
        helpful_count: 0,
        harmful_count: 0,
        feedback_events: vec![],
        confidence_decay_half_life_days: 30,
        pinned: false,
        deprecated: false,
        replaced_by: None,
        deprecation_reason: None,
        source_bead_ids: vec![],
        source_run_ids: vec![],
        tags: vec![],
        effective_score: Some(1.0),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    db.insert_playbook_bullet(&bullet).unwrap();

    // Create a mock prompt manifest showing this bullet was injected
    let manifest_path = temp.path().join("manifest.json");
    let manifest = PromptManifest {
        prompt_id: grove_types::PromptId::new("pr-1"),
        bead_id: bead_id.clone(),
        run_id: run_id.clone(),
        session_id: Some(grove_types::SessionId::new("ses-1")),
        contract: grove_types::prompt::ExecutionContract::RetryRescue,
        created_at: Utc::now(),
        token_budget: Some(4000),
        estimated_tokens: 100,
        prompt_bytes: 500,
        trimmed: false,
        retry_delta_summary: None,
        retrieval_query: None,
        retrieval_ranking_summary: vec![],
        sections: vec![
            PromptManifestSection {
                ordinal: 1,
                kind: PromptSegmentKind::Playbook,
                heading: "Playbook Rules".to_string(),
                included: true,
                estimated_tokens: 10,
                char_count: 50,
                trim_reason: None,
                provenance: PromptSectionProvenance {
                    bullet_ids: vec![bullet_id.clone()],
                    ..Default::default()
                },
                preview: "preview".to_string(),
            }
        ],
    };
    fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();

    // Create a successful session outcome
    let mut outcome = SessionOutcome {
        session: grove_types::ClaudeSessionRecord {
            id: grove_types::SessionId::new("ses-1"),
            run_id: run_id.clone(),
            external_session_id: None,
            ordinal_in_run: 1,
            status: SessionStatus::Completed,
            started_at: Utc::now(),
            ended_at: Some(Utc::now() + chrono::Duration::seconds(150)),
            prompt_id: None,
            prompt_manifest_path: Some(manifest_path.to_str().unwrap().to_string()),
            prompt_bytes: 500,
            estimated_input_tokens: 100,
            estimated_output_tokens: 50,
            exit_code: Some(0),
            stop_reason: None,
            transcript_path: "transcript.jsonl".to_string(),
        },
        protocol_events: vec![],
        analysis: Default::default(),
        terminal_class: grove_types::SessionTerminalClass::Success,
        context_pressure_pct: None,
        context_pressure_level: grove_types::ContextPressureLevel::Ok,
        stdout_tail: vec![],
        stderr_tail: vec![],
    };

    // Apply the outcome feedback
    diary::apply_outcome_feedback(&mut db, &bead_id, &run_id, &outcome, false).unwrap();

    // Verify bullet got a helpful feedback event!
    let updated_bullet = db.get_playbook_bullet(&bullet_id).unwrap().unwrap();
    assert_eq!(updated_bullet.helpful_count, 1, "Should have received helpful feedback from Completed outcome");
    
    // Now simulate a crash and apply again
    outcome.session.status = SessionStatus::Crashed;
    outcome.session.ended_at = Some(Utc::now() + chrono::Duration::seconds(4000)); // over an hour
    outcome.analysis.warnings = vec!["warning".to_string(), "warning".to_string()]; // 2 errors
    diary::apply_outcome_feedback(&mut db, &bead_id, &run_id, &outcome, true).unwrap();

    let crashed_bullet = db.get_playbook_bullet(&bullet_id).unwrap().unwrap();
    assert_eq!(crashed_bullet.harmful_count, 1, "Should have received harmful feedback from Crash");
}

// 2. Exact and approximate deduplication
#[test]
fn test_lesson_deduplication() {
    let temp = TempDir::new().unwrap();
    let db_path = temp.path().join("grove.db");
    let db_path = camino::Utf8PathBuf::from_path_buf(db_path).unwrap();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();

    let bead_id = BeadId::new("test-1");
    let run_id = RunId::new("run-1");

    let lessons = vec![
        "Validate inputs thoroughly before dispatching commands.".to_string(),
    ];

    // First ingestion creates it
    let created = lesson_ingest::ingest_lessons(&mut db, &bead_id, &run_id, &lessons).unwrap();
    assert_eq!(created, 1);

    // Exact duplicate does not create a new bullet
    let duplicate_created = lesson_ingest::ingest_lessons(&mut db, &bead_id, &run_id, &lessons).unwrap();
    assert_eq!(duplicate_created, 0);

    // Approximate duplicate (Jaccard > 0.75) does not create a new bullet either
    let approx = vec![
        "Validate inputs thoroughly before dispatching any commands.".to_string(), // Just added 'any'
    ];
    let approx_created = lesson_ingest::ingest_lessons(&mut db, &bead_id, &run_id, &approx).unwrap();
    assert_eq!(approx_created, 0);

    // Verify it received helpful points for the duplicate ingestions
    let bullets = db.list_non_retired_playbook_bullets(None).unwrap();
    assert_eq!(bullets.len(), 1);
    assert_eq!(bullets[0].helpful_count, 3); // 1 initial + 2 reinforces
}

// 3. Anti-pattern inversion and pruning
#[test]
fn test_scoring_anti_pattern_inversion() {
    let temp = TempDir::new().unwrap();
    let db_path = temp.path().join("grove.db");
    let db_path = camino::Utf8PathBuf::from_path_buf(db_path).unwrap();
    let mut db = Database::open(&db_path).unwrap();
    db.migrate().unwrap();

    let run_id = RunId::new("run-1");

    let bullet_id = grove_types::BulletId::new("blt-fail");
    let mut bullet = PlaybookBulletRecord {
        id: bullet_id.clone(),
        scope: BulletScope::Global,
        scope_key: None,
        category: "test".to_string(),
        text: "Always hardcode file paths".to_string(),
        bullet_type: BulletType::Rule,
        state: BulletState::Active,
        maturity: BulletMaturity::Established,
        helpful_count: 1,
        harmful_count: 10, // hugely harmful
        feedback_events: vec![
            grove_types::playbook::FeedbackEventRecord {
                kind: grove_types::playbook::FeedbackKind::Harmful,
                timestamp: Utc::now(),
                bead_id: None,
                run_id: Some(run_id.clone()),
                context: None,
                weight: 5.0,
            }
        ],
        confidence_decay_half_life_days: 30,
        pinned: false,
        deprecated: false,
        replaced_by: None,
        deprecation_reason: None,
        source_bead_ids: vec![],
        source_run_ids: vec![],
        tags: vec![],
        effective_score: Some(-5.0), // deeply negative
        created_at: Utc::now() - chrono::Duration::days(5),
        updated_at: Utc::now(),
    };
    db.insert_playbook_bullet(&bullet).unwrap();

    // Run scoring
    let config = ScoringConfig {
        prune_threshold: -3.0,
        min_events_for_promotion: 3,
        ..Default::default()
    };
    scoring::run_scoring_pass(&mut db, &config).unwrap();

    // Bullet should now be deprecated/retired
    let old_bullet = db.get_playbook_bullet(&bullet_id).unwrap().unwrap();
    assert_eq!(old_bullet.state, BulletState::Retired);
    assert_eq!(old_bullet.maturity, BulletMaturity::Deprecated);

    // But wait, there should be a new inverted rule in draft!
    let all_bullets = db.list_non_retired_bullets().unwrap();
    assert!(!all_bullets.is_empty(), "Inverted AntiPattern should exist");
    
    let text = &all_bullets[0].1;
    assert!(text.contains("AVOID: Always hardcode file paths"), "Should invert text string");
}

// 4. Explainable curation stays budget-aware
#[test]
fn test_phase6_curation_is_explainable_and_compact() {
    let input = PromptMaterializationInput {
        prompt_id: grove_types::PromptId::new("prompt-compact"),
        bead_id: BeadId::new("grove-test"),
        run_id: RunId::new("run-compact"),
        created_at: Utc::now(),
        contract: grove_types::ExecutionContract::SingleTask,
        task_title: "Compact prompt".to_string(),
        task_description: "Keep playbook injection bounded.".to_string(),
        reservation_hints: vec![],
        parent_handoffs: vec![],
        checkpoint: None,
        protocol_block: "[GROVE PROTOCOL]\nGROVE_EXIT: true".to_string(),
        rescue_card: None,
        token_budget: Some(35),
        retry_delta_summary: None,
        retrieval_query: None,
        archive_bundle: None,
        playbook_rules: vec![
            PlaybookBulletRecord {
                id: grove_types::BulletId::new("bullet-high"),
                scope: BulletScope::Global,
                scope_key: None,
                category: "workflow".to_string(),
                text: "High value guidance".to_string(),
                bullet_type: BulletType::Rule,
                state: BulletState::Active,
                maturity: BulletMaturity::Established,
                helpful_count: 10,
                harmful_count: 0,
                feedback_events: vec![],
                confidence_decay_half_life_days: 30,
                pinned: false,
                deprecated: false,
                replaced_by: None,
                deprecation_reason: None,
                source_bead_ids: vec![],
                source_run_ids: vec![],
                tags: vec![],
                effective_score: Some(4.0),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            PlaybookBulletRecord {
                id: grove_types::BulletId::new("bullet-low"),
                scope: BulletScope::Global,
                scope_key: None,
                category: "workflow".to_string(),
                text: "Low value guidance that should be trimmed under pressure".to_string(),
                bullet_type: BulletType::Rule,
                state: BulletState::Active,
                maturity: BulletMaturity::Candidate,
                helpful_count: 1,
                harmful_count: 0,
                feedback_events: vec![],
                confidence_decay_half_life_days: 30,
                pinned: false,
                deprecated: false,
                replaced_by: None,
                deprecation_reason: None,
                source_bead_ids: vec![],
                source_run_ids: vec![],
                tags: vec![],
                effective_score: Some(0.5),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        ],
    };
    let materialized = materialize_prompt(input);

    let playbook_sections: Vec<_> = materialized
        .manifest
        .sections
        .iter()
        .filter(|section| section.kind == PromptSegmentKind::Playbook)
        .collect();
    assert_eq!(playbook_sections.len(), 2, "both playbook bullets should be represented in the manifest");

    let trimmed_playbook = playbook_sections
        .iter()
        .filter(|section| section.trim_reason == Some(grove_types::PromptTrimReason::LowerPriorityPlaybookBullet))
        .count();
    assert!(trimmed_playbook >= 1, "low-priority playbook bullets should trim under pressure");
    assert!(
        playbook_sections.iter().all(|section| !section.provenance.bullet_ids.is_empty()),
        "playbook sections should remain explainable via bullet provenance even when trimmed"
    );
}
