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
mod tests;
