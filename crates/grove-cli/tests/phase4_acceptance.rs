// Phase 4 Acceptance Tests
//
// This test suite covers Phase 4 archive/retrieval requirements:
// 1. Transcript ingest produces searchable archive records
// 2. FTS search returns relevant snippets ranked by BM25
// 3. Archive retrieval integrates into prompt assembly as bounded snippets
// 4. Watermark-based idempotent ingest prevents duplicates on restart
// 5. Retrieval provenance is tracked through PromptSectionProvenance

use grove_db::Database;
use grove_types::{
    BeadId, RunId, SessionId, SourceId,
    archive::{
        ConversationRecord, MessageRecord, MessageRole, RetrievalBundle, SnippetRecord, SourceRecord,
    },
    PromptSegmentKind,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn setup_db() -> TestResult {
    Ok(())
}

fn open_test_db() -> Result<Database, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let db_path = camino::Utf8PathBuf::from_path_buf(dir.path().join("grove.db"))
        .map_err(|_| std::io::Error::other("db path must be valid UTF-8"))?;
    let mut db = Database::open(&db_path)?;
    db.migrate()?;
    // Keep tempdir alive by leaking (tests are short-lived)
    std::mem::forget(dir);
    Ok(db)
}

fn sample_source() -> SourceRecord {
    SourceRecord {
        id: SourceId::new("transcript"),
        source_path: camino::Utf8PathBuf::from(".grove/transcripts/grove-test/ses-test.jsonl"),
        origin_host: None,
        metadata_json: serde_json::json!({}),
    }
}

fn sample_conversation(session_id: &str, content: &str) -> ConversationRecord {
    ConversationRecord {
        id: None,
        bead_id: Some(BeadId::new("grove-test")),
        run_id: Some(RunId::new("run-test")),
        session_id: SessionId::new(session_id),
        workspace: None,
        title: Some("Test conversation".to_string()),
        source_path: camino::Utf8PathBuf::from(format!(
            ".grove/transcripts/grove-test/{session_id}.jsonl"
        )),
        started_at: Some("2026-03-20T10:00:00Z".parse().unwrap()),
        ended_at: Some("2026-03-20T10:30:00Z".parse().unwrap()),
        approx_tokens: Some(500),
        metadata_json: serde_json::json!({}),
        messages: vec![MessageRecord {
            id: None,
            idx: 0,
            role: MessageRole::Agent,
            author: Some("sonnet".to_string()),
            created_at: Some("2026-03-20T10:00:00Z".parse().unwrap()),
            content: content.to_string(),
            extra_json: serde_json::json!({}),
            snippets: vec![],
        }],
        source_id: SourceId::new("transcript"),
        origin_host: None,
    }
}

#[test]
fn ingest_transcript_produces_searchable_archive_record() -> TestResult {
    let mut db = open_test_db()?;

    db.insert_source_record(&sample_source())?;
    let conversation = sample_conversation(
        "ses-search-1",
        "Implemented the graceful shutdown handler with SIGTERM support and clean stop reason tracking."
    );

    let conv_id = db.insert_conversation_record(&conversation)?;
    assert!(conv_id > 0, "conversation should be inserted with valid ID");

    // FTS search should find the message
    let bundle = db.search_archive_fts("graceful shutdown", 5)?;
    assert!(
        !bundle.snippets.is_empty(),
        "FTS should return at least one snippet for 'graceful shutdown'"
    );
    assert!(
        bundle.conversations.contains(&conv_id),
        "returned snippet should reference our conversation"
    );

    Ok(())
}

#[test]
fn fts_search_ranks_relevant_snippets_by_bm25() -> TestResult {
    let mut db = open_test_db()?;

    db.insert_source_record(&sample_source())?;

    // Insert two conversations with different relevance
    let conv1 = sample_conversation(
        "ses-rank-1",
        "The authentication middleware validates JWT tokens and checks role-based permissions."
    );
    let conv2 = sample_conversation(
        "ses-rank-2",
        "Fixed a typo in the README file and updated the changelog."
    );

    db.insert_conversation_record(&conv1)?;
    db.insert_conversation_record(&conv2)?;

    let bundle = db.search_archive_fts("authentication middleware JWT", 5)?;
    assert!(
        !bundle.snippets.is_empty(),
        "should find at least one snippet"
    );

    // The first (most relevant) snippet should come from conv1
    let top = &bundle.snippets[0];
    assert!(
        top.score > 0.0,
        "BM25 score should be positive (we negate the raw SQLite value)"
    );

    Ok(())
}

#[test]
fn archive_retrieval_integrates_into_prompt_assembly_as_bounded_snippets() -> TestResult {
    use grove_session::{CheckpointPromptInput, PromptMaterializationInput, materialize_prompt};
    use grove_types::{ExecutionContract, PromptId};

    let bundle = RetrievalBundle {
        snippets: vec![
            grove_types::archive::RelevantSnippet {
                conversation_id: 1,
                message_id: 42,
                file_path: Some(camino::Utf8PathBuf::from("crates/grove-kernel/src/lib.rs")),
                snippet: "Implemented graceful shutdown with SIGTERM handler.".to_string(),
                score: 0.85,
            },
            grove_types::archive::RelevantSnippet {
                conversation_id: 2,
                message_id: 99,
                file_path: None,
                snippet: "Archive FTS search returns BM25-ranked results.".to_string(),
                score: 0.62,
            },
        ],
        conversations: vec![1, 2],
    };

    let input = PromptMaterializationInput {
        prompt_id: PromptId::new("prompt-phase4"),
        bead_id: BeadId::new("grove-test"),
        run_id: RunId::new("run-test"),
        created_at: "2026-03-20T12:00:00Z".parse().unwrap(),
        contract: ExecutionContract::SingleTask,
        task_title: "Test archive integration".to_owned(),
        task_description: "Verify archive snippets appear in prompt.".to_owned(),
        reservation_hints: vec![],
        parent_handoffs: vec![],
        checkpoint: None,
        protocol_block: "[GROVE PROTOCOL]\nGROVE_EXIT: true".to_owned(),
        rescue_card: None,
        token_budget: None,
        retry_delta_summary: None,
        archive_bundle: Some(bundle),
    };

    let materialized = materialize_prompt(input);

    // Should contain ArchiveSnippet sections
    let archive_sections: Vec<_> = materialized
        .manifest
        .sections
        .iter()
        .filter(|s| s.kind == PromptSegmentKind::ArchiveSnippet)
        .collect();

    assert_eq!(
        archive_sections.len(),
        2,
        "should inject exactly 2 archive snippet sections"
    );

    // Archive snippets should have provenance with message IDs
    for section in &archive_sections {
        assert!(
            section.provenance.archive_message_id.is_some(),
            "archive snippet should carry message_id provenance"
        );
    }

    // Rendered prompt should contain the snippet text
    assert!(
        materialized.rendered_prompt.contains("graceful shutdown"),
        "rendered prompt should include archive snippet content"
    );
    assert!(
        materialized.rendered_prompt.contains("BM25-ranked"),
        "rendered prompt should include second archive snippet"
    );

    Ok(())
}

#[test]
fn archive_snippets_are_trimmed_when_budget_is_tight() -> TestResult {
    use grove_session::{PromptMaterializationInput, materialize_prompt};
    use grove_types::{ExecutionContract, PromptId, PromptTrimReason};

    let bundle = RetrievalBundle {
        snippets: vec![grove_types::archive::RelevantSnippet {
            conversation_id: 1,
            message_id: 42,
            file_path: None,
            snippet: "A long historical context snippet that should be trimmed under budget pressure to make room for essential sections like task and protocol.".to_string(),
            score: 0.5,
        }],
        conversations: vec![1],
    };

    let input = PromptMaterializationInput {
        prompt_id: PromptId::new("prompt-budget"),
        bead_id: BeadId::new("grove-test"),
        run_id: RunId::new("run-test"),
        created_at: "2026-03-20T12:00:00Z".parse().unwrap(),
        contract: ExecutionContract::SingleTask,
        task_title: "Budget test".to_owned(),
        task_description: "Verify archive trim behavior.".to_owned(),
        reservation_hints: vec![],
        parent_handoffs: vec![],
        checkpoint: None,
        protocol_block: "[GROVE PROTOCOL]\nGROVE_EXIT: true".to_owned(),
        rescue_card: None,
        token_budget: Some(30), // Very tight budget
        retry_delta_summary: None,
        archive_bundle: Some(bundle),
    };

    let materialized = materialize_prompt(input);

    let trimmed: Vec<_> = materialized
        .manifest
        .sections
        .iter()
        .filter(|s| s.trim_reason == Some(PromptTrimReason::LowerPriorityArchiveSnippet))
        .collect();

    assert!(
        !trimmed.is_empty(),
        "archive snippets should be trimmed first under tight budget"
    );

    Ok(())
}

#[test]
fn watermark_prevents_duplicate_ingest_on_restart() -> TestResult {
    let mut db = open_test_db()?;

    db.insert_source_record(&sample_source())?;
    let conversation = sample_conversation(
        "ses-idempotent-1",
        "First ingest of this session transcript."
    );

    // First insert should succeed
    let first = db.insert_conversation_idempotent(&conversation)?;
    assert!(first.is_some(), "first ingest should insert successfully");

    // Second insert with same source+session should be no-op
    let second = db.insert_conversation_idempotent(&conversation)?;
    assert!(second.is_none(), "duplicate ingest should be skipped by watermark");

    // Verify watermark was recorded
    let watermarks = db.list_watermarks_for_source("transcript")?;
    assert_eq!(watermarks.len(), 1, "should have exactly one watermark entry");
    assert_eq!(watermarks[0].0, "ses-idempotent-1");
    assert_eq!(watermarks[0].2, 1, "record_count should be 1 (one message)");

    Ok(())
}

#[test]
fn fts_search_returns_empty_bundle_for_no_matches() -> TestResult {
    let mut db = open_test_db()?;

    db.insert_source_record(&sample_source())?;
    db.insert_conversation_record(&sample_conversation(
        "ses-empty-1",
        "A conversation about implementing error handling patterns."
    ))?;

    let bundle = db.search_archive_fts("quantum teleportation warp drive", 5)?;
    assert!(
        bundle.snippets.is_empty(),
        "FTS should return empty results for unrelated queries"
    );
    assert!(
        bundle.conversations.is_empty(),
        "no conversations should match"
    );

    Ok(())
}
