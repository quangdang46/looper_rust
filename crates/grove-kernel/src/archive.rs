use anyhow::Result;
use grove_session::TranscriptReplay;
use grove_types::{
    BeadId, RunId, SessionId,
    archive::{ConversationRecord, MessageRecord, MessageRole, SnippetRecord},
    TranscriptEvent,
};
use regex::Regex;

/// Normalizes a raw session transcript into an Archive Conversation Record.
pub fn ingest_transcript_to_archive(
    bead_id: BeadId,
    run_id: RunId,
    session_id: SessionId,
    transcript: &TranscriptReplay,
) -> Result<ConversationRecord> {
    let mut started_at = None;
    let mut ended_at = None;

    let mut agent_lines = Vec::new();

    // In a typical Grove session, the prompt is implicitly the context,
    // and the actual Claude process emits stdout lines. We group all stdout
    // into a single agent message for archiving context.
    for event in &transcript.events {
        match event {
            TranscriptEvent::SessionStarted { ts, .. } => {
                if started_at.is_none() {
                    started_at = Some(ts.clone());
                }
            }
            TranscriptEvent::SessionEnded { ts, .. } => {
                ended_at = Some(ts.clone());
            }
            TranscriptEvent::StdoutLine { line, .. } => {
                agent_lines.push(line.clone());
            }
            // ParsedProtocol events are emitted dynamically in the stream
            _ => {}
        }
    }

    let joined_content = agent_lines.join("\n");
    let snippets = extract_markdown_snippets(&joined_content);

    let messages = vec![MessageRecord {
        id: None,
        idx: 0,
        role: MessageRole::Agent,
        author: Some("sonnet".to_string()),
        created_at: started_at.clone(),
        content: joined_content,
        extra_json: serde_json::json!({}),
        snippets,
    }];

    Ok(ConversationRecord {
        id: None,
        bead_id: Some(bead_id),
        run_id: Some(run_id),
        session_id,
        workspace: None,
        title: None,
        source_path: path_for_transcript(), // Dummy, replace correctly
        started_at,
        ended_at,
        approx_tokens: None,
        metadata_json: serde_json::json!({}),
        messages,
        source_id: grove_types::SourceId::new("transcript"),
        origin_host: None,
    })
}

// Dummy helper
fn path_for_transcript() -> camino::Utf8PathBuf {
    camino::Utf8PathBuf::from("/dev/null")
}

fn extract_markdown_snippets(content: &str) -> Vec<SnippetRecord> {
    let mut snippets = Vec::new();
    let re = Regex::new(r"(?s)```(\w+)?\n(.*?)```").expect("valid regex");

    for cap in re.captures_iter(content) {
        let language = cap.get(1).map(|m: regex::Match<'_>| m.as_str().to_string());
        let snippet_text = cap.get(2).map(|m: regex::Match<'_>| m.as_str().to_string());
        snippets.push(SnippetRecord {
            id: None,
            file_path: None,
            start_line: None,
            end_line: None,
            language,
            snippet_text,
        });
    }

    snippets
}
