
use super::*;

#[test]
fn parse_repeated_result_overwrites() {
    let mut parser = ProtocolParser::default();

    assert!(matches!(
        parser.parse_stdout_line("GROVE_RESULT: first pass"),
        ParserLineKind::Protocol(ProtocolEvent::Result { .. })
    ));
    assert!(matches!(
        parser.parse_stdout_line("GROVE_RESULT: final pass"),
        ParserLineKind::Protocol(ProtocolEvent::Result { .. })
    ));

    assert_eq!(parser.state().result_summary.as_deref(), Some("final pass"));
    assert_eq!(parser.state().events.len(), 2);
}

#[test]
fn parse_repeated_artifacts_merges() {
    let mut parser = ProtocolParser::default();

    parser.parse_stdout_line("GROVE_ARTIFACTS: src/lib.rs, tests/lib.rs");
    parser.parse_stdout_line("GROVE_ARTIFACTS: tests/lib.rs, src/main.rs");

    assert_eq!(
        parser.state().artifacts,
        vec![
            "src/lib.rs".to_owned(),
            "tests/lib.rs".to_owned(),
            "src/main.rs".to_owned(),
        ]
    );
}

#[test]
fn parse_malformed_marker_logs_warning() {
    let mut parser = ProtocolParser::default();

    let line = parser.parse_stdout_line("GROVE_EXIT: maybe");

    assert_eq!(
        line,
        ParserLineKind::PlainStdout("GROVE_EXIT: maybe".to_owned())
    );
    assert_eq!(parser.warnings().len(), 1);
    assert!(
        parser.warnings()[0]
            .reason
            .contains("invalid GROVE_EXIT value")
    );
    assert!(parser.state().explicit_exit.is_none());
}

#[test]
fn parse_unknown_grove_marker_logs_warning() {
    let mut parser = ProtocolParser::default();

    parser.parse_stdout_line("GROVE_UNKNOWN: value");

    assert_eq!(parser.warnings().len(), 1);
    assert!(parser.warnings()[0].reason.contains("unknown GROVE marker"));
}

#[test]
fn parse_stderr_line_keeps_plain_text() {
    let parser = ProtocolParser::default();
    let line = parser.parse_stderr_line("permission denied");
    assert_eq!(
        line,
        ParserLineKind::PlainStderr("permission denied".to_owned())
    );
}

#[test]
fn parse_checkpoint_updates_latest_checkpoint() {
    let mut parser = ProtocolParser::default();

    parser.parse_stdout_line(
            "GROVE_CHECKPOINT: {\"progress\":\"halfway\",\"next_step\":\"finish\",\"context\":{},\"open_questions\":[],\"claimed_paths\":[]}",
        );

    assert_eq!(
        parser
            .state()
            .latest_checkpoint
            .as_ref()
            .map(|payload| payload.progress.as_str()),
        Some("halfway")
    );
}

#[test]
fn multiline_decisions_are_captured_after_empty_header() {
    let mut parser = ProtocolParser::default();

    assert!(matches!(
        parser.parse_stdout_line("GROVE_DECISIONS:"),
        ParserLineKind::Protocol(ProtocolEvent::Decisions { .. })
    ));
    assert!(matches!(
        parser.parse_stdout_line("- kept implementation minimal"),
        ParserLineKind::Protocol(ProtocolEvent::Decisions { .. })
    ));
    assert!(matches!(
        parser.parse_stdout_line("- reused existing helper"),
        ParserLineKind::Protocol(ProtocolEvent::Decisions { .. })
    ));

    assert_eq!(
        parser.state().decisions,
        vec![
            "kept implementation minimal".to_owned(),
            "reused existing helper".to_owned(),
        ]
    );
}

#[test]
fn multiline_warnings_stop_on_blank_line() {
    let mut parser = ProtocolParser::default();

    parser.parse_stdout_line("GROVE_WARNINGS:");
    parser.parse_stdout_line("- first warning");
    parser.parse_stdout_line("");
    let plain = parser.parse_stdout_line("- not a warning anymore");

    assert_eq!(
        plain,
        ParserLineKind::PlainStdout("- not a warning anymore".to_owned())
    );
    assert_eq!(parser.state().warnings, vec!["first warning".to_owned()]);
}
