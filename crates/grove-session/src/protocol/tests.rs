use super::*;
use std::error::Error;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn parse_grove_result() -> TestResult {
    let event = parse_protocol_event("GROVE_RESULT: task complete")?.ok_or("missing event")?;
    assert_eq!(
        event,
        ProtocolEvent::Result {
            summary: "task complete".to_owned()
        }
    );
    Ok(())
}

#[test]
fn parse_grove_result_with_whitespace() -> TestResult {
    let event =
        parse_protocol_event("   GROVE_RESULT:  task complete  ")?.ok_or("missing event")?;
    assert_eq!(
        event,
        ProtocolEvent::Result {
            summary: "task complete".to_owned()
        }
    );
    Ok(())
}

#[test]
fn parse_grove_exit_true() -> TestResult {
    let event = parse_protocol_event("GROVE_EXIT: true")?.ok_or("missing event")?;
    assert_eq!(event, ProtocolEvent::Exit { value: true });
    Ok(())
}

#[test]
fn parse_grove_exit_false() -> TestResult {
    let event = parse_protocol_event("GROVE_EXIT: false")?.ok_or("missing event")?;
    assert_eq!(event, ProtocolEvent::Exit { value: false });
    Ok(())
}

#[test]
fn parse_grove_exit_case_insensitive() -> TestResult {
    let event = parse_protocol_event("GROVE_EXIT: FALSE")?.ok_or("missing event")?;
    assert_eq!(event, ProtocolEvent::Exit { value: false });
    Ok(())
}

#[test]
fn parse_grove_exit_invalid_value() {
    let error = match parse_protocol_event("GROVE_EXIT: maybe") {
        Ok(value) => panic!("expected parse error, got {value:?}"),
        Err(error) => error,
    };
    assert!(matches!(error, ProtocolParseError::InvalidExitValue { .. }));
}

#[test]
fn parse_grove_artifacts_comma_separated() -> TestResult {
    let event = parse_protocol_event("GROVE_ARTIFACTS: src/lib.rs, tests/lib.rs")?
        .ok_or("missing event")?;
    assert_eq!(
        event,
        ProtocolEvent::Artifacts {
            items: vec!["src/lib.rs".to_owned(), "tests/lib.rs".to_owned()],
        }
    );
    Ok(())
}

#[test]
fn parse_grove_artifacts_json_array() -> TestResult {
    let event = parse_protocol_event("GROVE_ARTIFACTS: [\"src/lib.rs\", \"tests/lib.rs\"]")?
        .ok_or("missing event")?;
    assert_eq!(
        event,
        ProtocolEvent::Artifacts {
            items: vec!["src/lib.rs".to_owned(), "tests/lib.rs".to_owned()],
        }
    );
    Ok(())
}

#[test]
fn parse_grove_artifacts_single_item() -> TestResult {
    let event = parse_protocol_event("GROVE_ARTIFACTS: src/lib.rs")?.ok_or("missing event")?;
    assert_eq!(
        event,
        ProtocolEvent::Artifacts {
            items: vec!["src/lib.rs".to_owned()],
        }
    );
    Ok(())
}

#[test]
fn parse_grove_artifacts_empty() -> TestResult {
    let event = parse_protocol_event("GROVE_ARTIFACTS: none")?.ok_or("missing event")?;
    assert_eq!(event, ProtocolEvent::Artifacts { items: Vec::new() });
    Ok(())
}

#[test]
fn parse_grove_lessons_comma_separated() -> TestResult {
    let event = parse_protocol_event("GROVE_LESSONS: validate inputs, keep paths narrow")?
        .ok_or("missing event")?;
    assert_eq!(
        event,
        ProtocolEvent::Lessons {
            items: vec!["validate inputs".to_owned(), "keep paths narrow".to_owned()],
        }
    );
    Ok(())
}

#[test]
fn parse_grove_lessons_json_array() -> TestResult {
    let event =
        parse_protocol_event("GROVE_LESSONS: [\"validate inputs\", \"keep paths narrow\"]")?
            .ok_or("missing event")?;
    assert_eq!(
        event,
        ProtocolEvent::Lessons {
            items: vec!["validate inputs".to_owned(), "keep paths narrow".to_owned()],
        }
    );
    Ok(())
}

#[test]
fn parse_grove_decisions_comma_separated() -> TestResult {
    let event = parse_protocol_event("GROVE_DECISIONS: use line markers, keep protocol strict")?
        .ok_or("missing event")?;
    assert_eq!(
        event,
        ProtocolEvent::Decisions {
            items: vec![
                "use line markers".to_owned(),
                "keep protocol strict".to_owned()
            ],
        }
    );
    Ok(())
}

#[test]
fn parse_grove_warnings_comma_separated() -> TestResult {
    let event = parse_protocol_event("GROVE_WARNINGS: mirror pending, follow-up required")?
        .ok_or("missing event")?;
    assert_eq!(
        event,
        ProtocolEvent::Warnings {
            items: vec!["mirror pending".to_owned(), "follow-up required".to_owned()],
        }
    );
    Ok(())
}

#[test]
fn parse_grove_checkpoint_valid_json() -> TestResult {
    let event = parse_protocol_event(
        "GROVE_CHECKPOINT: {\"progress\":\"halfway\",\"next_step\":\"finish\",\"context\":{},\"open_questions\":[],\"claimed_paths\":[],\"confidence\":0.5}",
    )?
    .ok_or("missing event")?;
    assert!(matches!(event, ProtocolEvent::Checkpoint { .. }));
    Ok(())
}

#[test]
fn parse_grove_checkpoint_minimal_json() -> TestResult {
    let event = parse_protocol_event(
        "GROVE_CHECKPOINT: {\"progress\":\"halfway\",\"next_step\":\"finish\",\"context\":{},\"open_questions\":[],\"claimed_paths\":[]}",
    )?
    .ok_or("missing event")?;
    assert!(matches!(event, ProtocolEvent::Checkpoint { .. }));
    Ok(())
}

#[test]
fn parse_grove_checkpoint_invalid_json() {
    let error = match parse_protocol_event("GROVE_CHECKPOINT: {not json}") {
        Ok(value) => panic!("expected parse error, got {value:?}"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ProtocolParseError::InvalidCheckpointJson { .. }
    ));
}

#[test]
fn parse_grove_checkpoint_with_context() -> TestResult {
    let event = parse_protocol_event(
        "GROVE_CHECKPOINT: {\"progress\":\"halfway\",\"next_step\":\"finish\",\"context\":{\"resume\":true},\"open_questions\":[\"ship?\"],\"claimed_paths\":[\"src/**\"],\"confidence\":0.8}",
    )?
    .ok_or("missing event")?;
    let ProtocolEvent::Checkpoint { payload } = event else {
        panic!("expected checkpoint event");
    };
    assert_eq!(payload.open_questions, vec!["ship?".to_owned()]);
    assert_eq!(payload.claimed_paths, vec!["src/**".to_owned()]);
    Ok(())
}

#[test]
fn parse_plain_line_no_marker() -> TestResult {
    assert!(parse_protocol_event("working on tests")?.is_none());
    Ok(())
}

#[test]
fn parse_line_with_marker_in_middle() -> TestResult {
    assert!(parse_protocol_event("done soon GROVE_EXIT: true")?.is_none());
    Ok(())
}

#[test]
fn parse_marker_with_leading_whitespace() -> TestResult {
    let event = parse_protocol_event("\t GROVE_EXIT: true")?.ok_or("missing event")?;
    assert_eq!(event, ProtocolEvent::Exit { value: true });
    Ok(())
}

#[test]
fn parse_marker_case_sensitive() -> TestResult {
    assert!(parse_protocol_event("grove_exit: true")?.is_none());
    Ok(())
}
