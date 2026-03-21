// Phase 5 Acceptance Tests
//
// This test suite covers Phase 5 basic playbook memory requirements:
// 1. Repeated lessons become active rules
// 2. One-off noisy lessons remain weak candidates
// 3. No external memory tool dependency is required (pure DB)
// 4. Verification runs before final mirror

use grove_db::Database;
use grove_kernel::lesson_ingest::ingest_lessons;
use grove_kernel::scoring::{ScoringConfig, run_scoring_pass};
use grove_types::{
    BeadId, RunId,
    playbook::{BulletMaturity, BulletState, PlaybookBulletRecord},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn open_test_db() -> Result<Database, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    // Keep tempdir alive for test duration
    std::mem::forget(dir);
    Ok(db)
}

#[test]
fn repeated_lessons_become_active_rules() -> TestResult {
    let mut db = open_test_db()?;

    let bead_id = BeadId::new("bead-1");
    let target_lesson = "Always validate inputs before making external requests";

    // First ingestion (Run 1)
    let run1 = RunId::new("run-1");
    let changed1 = ingest_lessons(&mut db, &bead_id, &run1, &[target_lesson.to_string()])?;
    assert_eq!(changed1, 1, "First ingest should create 1 new bullet");

    // After 1 run, it should be a Candidate
    let score_config = ScoringConfig::default();
    run_scoring_pass(&mut db, &score_config)?;

    let active = db.list_active_bullets(None)?; // but it's Draft so list_active_bullets won't show it!
    assert!(active.is_empty(), "Candidates don't appear as active until promoted");

    // Run 2 and 3 and 4: repeated ingestion (same hash) reinforces the bullet
    for i in 2..=5 {
        let run_id = RunId::new(format!("run-{i}"));
        let changed = ingest_lessons(&mut db, &bead_id, &run_id, &[target_lesson.to_string()])?;
        assert_eq!(changed, 0, "Subsequent ingests should reinforce, not create new");
    }

    // Now it has 5 helpful events. Let's run the scoring pass to promote it.
    run_scoring_pass(&mut db, &score_config)?;

    // It should now be promoted to Established/Active
    let active_now = db.list_active_bullets(None)?;
    assert_eq!(active_now.len(), 1, "Promoted bullet should now be active");
    
    let bullet = &active_now[0];
    assert_eq!(bullet.maturity, BulletMaturity::Established, "Should be promoted to Established");
    assert_eq!(bullet.state, BulletState::Active, "State should be active");

    Ok(())
}

#[test]
fn one_off_noisy_lessons_remain_weak_candidates() -> TestResult {
    let mut db = open_test_db()?;

    let bead_id = BeadId::new("bead-noise");
    let noisy_lesson = "I think I should use red font for this specific button";

    let run1 = RunId::new("run-noise-1");
    ingest_lessons(&mut db, &bead_id, &run1, &[noisy_lesson.to_string()])?;

    // Run scoring pass
    let score_config = ScoringConfig::default();
    run_scoring_pass(&mut db, &score_config)?;

    // It should not be active because it lacks sufficient events
    let active = db.list_active_bullets(None)?;
    assert!(active.is_empty(), "One-off noisy lesson should remain inactive draft/candidate");

    Ok(())
}

#[test]
fn pure_db_no_external_memory_tool_in_prompt_assembly() -> TestResult {
    use grove_session::{PromptMaterializationInput, materialize_prompt};
    use grove_types::{ExecutionContract, PromptId, PromptSegmentKind};
    
    // We demonstrate that passing a fully-formed PlaybookBulletRecord list
    // constructs the prompt segments entirely locally.
    
    let rule = PlaybookBulletRecord {
        id: grove_types::BulletId::new("blt-1"),
        scope: grove_types::playbook::BulletScope::Global,
        scope_key: None,
        category: "session_lesson".to_string(),
        text: "Use idiomatic Rust".to_string(),
        bullet_type: grove_types::playbook::BulletType::Rule,
        state: BulletState::Active,
        maturity: BulletMaturity::Established,
        helpful_count: 5,
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
        effective_score: Some(3.5),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    let input = PromptMaterializationInput {
        prompt_id: PromptId::new("prompt-test"),
        bead_id: BeadId::new("bead-test"),
        run_id: RunId::new("run-test"),
        created_at: "2026-03-20T12:00:00Z".parse().unwrap(),
        contract: ExecutionContract::SingleTask,
        task_title: "Test".to_string(),
        task_description: "Do the task".to_string(),
        reservation_hints: vec![],
        parent_handoffs: vec![],
        checkpoint: None,
        protocol_block: "[GROVE PROTOCOL]".to_string(),
        rescue_card: None,
        token_budget: None,
        retry_delta_summary: None,
        archive_bundle: None,
        playbook_rules: vec![rule],
    };

    let materialized = materialize_prompt(input);
    let rendered = &materialized.rendered_prompt;
    
    assert!(materialized.manifest.sections.iter().any(|section| {
        section.kind == PromptSegmentKind::Playbook
            && section.heading == "Playbook session_lesson (Maturity: Established)"
    }), "Manifest includes playbook segment");
    assert!(rendered.contains("[SESSION_LESSON] Use idiomatic Rust"), "Prompt includes actual rule text");
    
    Ok(())
}

#[test]
fn verification_mode_inferred_from_contract_and_workspace() -> TestResult {
    use grove_session::VerificationMode;
    use grove_types::ExecutionContract;

    let dir = tempfile::tempdir()?;
    let utf8_dir = camino::Utf8Path::from_path(dir.path()).unwrap();

    // Default with no build files
    let mode = VerificationMode::infer(ExecutionContract::SingleTask, utf8_dir);
    assert_eq!(mode, VerificationMode::ProtocolComplete);

    // With Cargo.toml
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
    let mode_rust = VerificationMode::infer(ExecutionContract::SingleTask, utf8_dir);
    assert_eq!(mode_rust, VerificationMode::RustCompileCheck);
    
    // Clean up
    std::fs::remove_file(dir.path().join("Cargo.toml")).unwrap();

    // With package.json
    std::fs::write(dir.path().join("package.json"), "{}").unwrap();
    let mode_node = VerificationMode::infer(ExecutionContract::SingleTask, utf8_dir);
    assert_eq!(mode_node, VerificationMode::NodeBuildCheck);

    Ok(())
}
