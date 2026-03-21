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
    let mut messages = Vec::new();

    for (idx, event) in transcript.events.iter().enumerate() {
        match event {
            TranscriptEvent::SessionStarted { ts, .. } => {
                if started_at.is_none() {
                    started_at = Some(ts.clone());
                }
            }
            TranscriptEvent::SessionEnded { ts, .. } => {
                ended_at = Some(ts.clone());
            }
            TranscriptEvent::StdoutLine { line, ts } => {
                messages.push(MessageRecord {
                    id: None,
                    idx: idx as i64,
                    role: MessageRole::Agent,
                    author: Some("sonnet".to_string()),
                    created_at: Some(ts.clone()),
                    content: line.clone(),
                    extra_json: serde_json::json!({ "kind": "stdout" }),
                    snippets: extract_markdown_snippets(line),
                });
            }
            TranscriptEvent::StderrLine { line, ts } => {
                messages.push(MessageRecord {
                    id: None,
                    idx: idx as i64,
                    role: MessageRole::System,
                    author: None,
                    created_at: Some(ts.clone()),
                    content: line.clone(),
                    extra_json: serde_json::json!({ "kind": "stderr" }),
                    snippets: extract_markdown_snippets(line),
                });
            }
            TranscriptEvent::ParsedProtocol { event, ts } => {
                let content = match event {
                    grove_types::ProtocolEvent::Result { summary } => format!("GROVE_RESULT: {summary}"),
                    grove_types::ProtocolEvent::Artifacts { items } => {
                        format!("GROVE_ARTIFACTS: {}", items.join(", "))
                    }
                    grove_types::ProtocolEvent::Lessons { items } => {
                        format!("GROVE_LESSONS: {}", items.join(" | "))
                    }
                    grove_types::ProtocolEvent::Decisions { items } => {
                        format!("GROVE_DECISIONS: {}", items.join(" | "))
                    }
                    grove_types::ProtocolEvent::Warnings { items } => {
                        format!("GROVE_WARNINGS: {}", items.join(" | "))
                    }
                    grove_types::ProtocolEvent::Exit { value } => format!("GROVE_EXIT: {value}"),
                    grove_types::ProtocolEvent::Checkpoint { payload } => format!(
                        "GROVE_CHECKPOINT: progress={} next_step={}",
                        payload.progress, payload.next_step
                    ),
                };
                messages.push(MessageRecord {
                    id: None,
                    idx: idx as i64,
                    role: MessageRole::System,
                    author: Some("grove-protocol".to_string()),
                    created_at: Some(ts.clone()),
                    content,
                    extra_json: serde_json::to_value(event).unwrap_or_else(|_| serde_json::json!({})),
                    snippets: Vec::new(),
                });
            }
        }
    }

    Ok(ConversationRecord {
        id: None,
        bead_id: Some(bead_id),
        run_id: Some(run_id),
        session_id,
        workspace: None,
        title: None,
        source_path: camino::Utf8PathBuf::from("."),
        started_at,
        ended_at,
        approx_tokens: None,
        metadata_json: serde_json::json!({ "message_count": messages.len() }),
        messages,
        source_id: grove_types::SourceId::new("transcript"),
        origin_host: None,
    })
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
