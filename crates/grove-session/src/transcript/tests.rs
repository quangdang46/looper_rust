use super::*;
use grove_types::{CheckpointPayload, ProtocolEvent};
use serde_json::Value;
use std::{error::Error, fs};
use tempfile::tempdir;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn transcript_writer_appends_jsonl_and_flushes_each_event() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("transcripts/bead/ses-1.jsonl");
    let session_id = SessionId::new("ses-1");
    let ts: Timestamp = "2026-03-17T08:00:00Z".parse()?;

    let mut writer = TranscriptWriter::open(&path)?;
    writer.append_session_started(session_id, ts)?;
    writer.append_stdout_line("Inspecting src/lib.rs", ts)?;
    writer.append_protocol_event(
        ProtocolEvent::Decisions {
            items: vec!["Keep transcript append-only".to_owned()],
        },
        ts,
    )?;
    writer.append_session_ended(Some(0), ts)?;

    let content = fs::read_to_string(&path)?;
    let lines: Vec<_> = content.lines().collect();
    assert_eq!(lines.len(), 4);

    let first: Value = serde_json::from_str(lines[0])?;
    assert_eq!(first["kind"], "session_started");
    assert_eq!(first["session_id"], "ses-1");

    let second: Value = serde_json::from_str(lines[1])?;
    assert_eq!(second["kind"], "stdout");
    assert_eq!(second["line"], "Inspecting src/lib.rs");

    let third: Value = serde_json::from_str(lines[2])?;
    assert_eq!(third["kind"], "protocol");
    assert_eq!(third["event"]["type"], "decision");
    assert_eq!(third["event"]["items"][0], "Keep transcript append-only");

    let fourth: Value = serde_json::from_str(lines[3])?;
    assert_eq!(fourth["kind"], "session_ended");
    assert_eq!(fourth["exit_code"], 0);
    Ok(())
}

#[test]
fn transcript_writer_preserves_append_semantics_on_reopen() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("transcripts/bead/ses-2.jsonl");
    let ts: Timestamp = "2026-03-17T08:00:00Z".parse()?;

    {
        let mut writer = TranscriptWriter::open(&path)?;
        writer.append_stdout_line("first", ts)?;
    }
    {
        let mut writer = TranscriptWriter::open(&path)?;
        writer.append_stderr_line("second", ts)?;
    }

    let content = fs::read_to_string(&path)?;
    let lines: Vec<_> = content.lines().collect();
    assert_eq!(lines.len(), 2);
    let first: Value = serde_json::from_str(lines[0])?;
    let second: Value = serde_json::from_str(lines[1])?;
    assert_eq!(first["kind"], "stdout");
    assert_eq!(second["kind"], "stderr");
    Ok(())
}

#[test]
fn transcript_writer_serializes_checkpoint_payload() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("transcripts/bead/ses-3.jsonl");
    let ts: Timestamp = "2026-03-17T08:00:00Z".parse()?;
    let mut writer = TranscriptWriter::open(&path)?;

    writer.append_protocol_event(
        ProtocolEvent::Checkpoint {
            payload: CheckpointPayload {
                progress: "halfway".to_owned(),
                next_step: "finish".to_owned(),
                context: serde_json::json!({"resume": true}),
                open_questions: vec!["ship?".to_owned()],
                claimed_paths: vec!["src/**".to_owned()],
                confidence: Some(0.8),
            },
        },
        ts,
    )?;

    let content = fs::read_to_string(&path)?;
    let line: Value = serde_json::from_str(content.lines().next().ok_or("missing line")?)?;
    assert_eq!(line["kind"], "protocol");
    assert_eq!(line["event"]["type"], "checkpoint");
    assert_eq!(line["event"]["payload"]["progress"], "halfway");
    assert_eq!(line["event"]["payload"]["claimed_paths"][0], "src/**");
    Ok(())
}

#[test]
fn replay_transcript_round_trips_written_events() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("transcripts/bead/ses-4.jsonl");
    let session_id = SessionId::new("ses-4");
    let ts: Timestamp = "2026-03-17T08:00:00Z".parse()?;

    let mut writer = TranscriptWriter::open(&path)?;
    writer.append_session_started(session_id.clone(), ts)?;
    writer.append_stdout_line("working", ts)?;
    writer.append_protocol_event(ProtocolEvent::Exit { value: true }, ts)?;
    writer.append_session_ended(Some(0), ts)?;

    let replay = replay_transcript(&path)?;
    assert_eq!(replay.events.len(), 4);
    assert!(matches!(
        &replay.events[0],
        TranscriptEvent::SessionStarted { session_id: actual, .. } if actual == &session_id
    ));
    assert!(matches!(
        &replay.events[1],
        TranscriptEvent::StdoutLine { line, .. } if line == "working"
    ));
    assert!(matches!(
        &replay.events[2],
        TranscriptEvent::ParsedProtocol {
            event: ProtocolEvent::Exit { value: true },
            ..
        }
    ));
    assert!(matches!(
        &replay.events[3],
        TranscriptEvent::SessionEnded {
            exit_code: Some(0),
            ..
        }
    ));
    Ok(())
}

#[test]
fn replay_transcript_rejects_unknown_kind() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("transcripts/bead/ses-5.jsonl");
    fs::create_dir_all(path.parent().ok_or("missing parent")?)?;
    fs::write(
        &path,
        "{\"ts\":\"2026-03-17T08:00:00Z\",\"kind\":\"mystery\"}\n",
    )?;

    let error = replay_transcript(&path).expect_err("expected invalid transcript line");
    match error {
        TranscriptError::InvalidLine { line, reason, .. } => {
            assert_eq!(line, 1);
            assert!(reason.contains("unknown transcript kind `mystery`"));
        }
        other => panic!("expected invalid line error, got {other:?}"),
    }
    Ok(())
}
