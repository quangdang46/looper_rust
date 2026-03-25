#![allow(clippy::unwrap_used, clippy::expect_used)]
use grove_types::{ProtocolEvent, ProtocolState};

use crate::protocol::parse_protocol_event;

#[derive(Debug, Clone, PartialEq)]
pub enum ParserLineKind {
    Protocol(ProtocolEvent),
    PlainStdout(String),
    PlainStderr(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolWarning {
    pub line: usize,
    pub raw_line: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingListKind {
    Artifacts,
    Lessons,
    Decisions,
    Warnings,
}

impl PendingListKind {
    fn from_event(event: &ProtocolEvent) -> Option<Self> {
        match event {
            ProtocolEvent::Artifacts { items } if items.is_empty() => Some(Self::Artifacts),
            ProtocolEvent::Lessons { items } if items.is_empty() => Some(Self::Lessons),
            ProtocolEvent::Decisions { items } if items.is_empty() => Some(Self::Decisions),
            ProtocolEvent::Warnings { items } if items.is_empty() => Some(Self::Warnings),
            _ => None,
        }
    }

    fn into_event(self, item: String) -> ProtocolEvent {
        match self {
            Self::Artifacts => ProtocolEvent::Artifacts { items: vec![item] },
            Self::Lessons => ProtocolEvent::Lessons { items: vec![item] },
            Self::Decisions => ProtocolEvent::Decisions { items: vec![item] },
            Self::Warnings => ProtocolEvent::Warnings { items: vec![item] },
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProtocolParser {
    state: ProtocolState,
    warnings: Vec<ProtocolWarning>,
    stdout_lines_seen: usize,
    pending_list: Option<PendingListKind>,
}

impl ProtocolParser {
    pub fn parse_stdout_line(&mut self, line: &str) -> ParserLineKind {
        self.stdout_lines_seen += 1;
        let trimmed = line.trim_start();

        if trimmed.starts_with("GROVE_") {
            self.pending_list = None;
            match parse_protocol_event(line) {
                Ok(Some(event)) => {
                    self.pending_list = PendingListKind::from_event(&event);
                    self.apply_event(event.clone());
                    ParserLineKind::Protocol(event)
                }
                Ok(None) => ParserLineKind::PlainStdout(line.to_owned()),
                Err(error) => {
                    self.warnings.push(ProtocolWarning {
                        line: self.stdout_lines_seen,
                        raw_line: line.to_owned(),
                        reason: error.to_string(),
                    });
                    ParserLineKind::PlainStdout(line.to_owned())
                }
            }
        } else if let Some(pending) = self.pending_list {
            if let Some(item) = parse_pending_list_item(trimmed) {
                let event = pending.into_event(item);
                self.apply_event(event.clone());
                ParserLineKind::Protocol(event)
            } else {
                if trimmed.is_empty() {
                    self.pending_list = None;
                }
                ParserLineKind::PlainStdout(line.to_owned())
            }
        } else {
            ParserLineKind::PlainStdout(line.to_owned())
        }
    }

    #[must_use]
    pub fn parse_stderr_line(&self, line: &str) -> ParserLineKind {
        ParserLineKind::PlainStderr(line.to_owned())
    }

    #[must_use]
    pub fn state(&self) -> &ProtocolState {
        &self.state
    }

    #[must_use]
    pub fn warnings(&self) -> &[ProtocolWarning] {
        &self.warnings
    }

    #[must_use]
    pub fn into_state(self) -> ProtocolState {
        self.state
    }

    fn apply_event(&mut self, event: ProtocolEvent) {
        match &event {
            ProtocolEvent::Result { summary } => {
                self.state.result_summary = Some(summary.clone());
            }
            ProtocolEvent::Artifacts { items } => merge_unique(&mut self.state.artifacts, items),
            ProtocolEvent::Lessons { items } => merge_unique(&mut self.state.lessons, items),
            ProtocolEvent::Decisions { items } => merge_unique(&mut self.state.decisions, items),
            ProtocolEvent::Warnings { items } => merge_unique(&mut self.state.warnings, items),
            ProtocolEvent::Exit { value } => {
                self.state.explicit_exit = Some(*value);
            }
            ProtocolEvent::Checkpoint { payload } => {
                self.state.latest_checkpoint = Some(payload.clone());
            }
        }
        self.state.events.push(event);
    }
}

fn merge_unique(target: &mut Vec<String>, incoming: &[String]) {
    for item in incoming {
        if !target.iter().any(|existing| existing == item) {
            target.push(item.clone());
        }
    }
}

fn parse_pending_list_item(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let item = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .map(str::trim)?;
    (!item.is_empty()).then(|| item.to_owned())
}

#[cfg(test)]
mod tests {
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
}
