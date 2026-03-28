#![allow(clippy::unwrap_used, clippy::expect_used)]
use grove_types::{CheckpointPayload, ProtocolEvent};
use serde_json::Value;
use thiserror::Error;

pub const GROVE_RESULT_PREFIX: &str = "GROVE_RESULT:";
pub const GROVE_ARTIFACTS_PREFIX: &str = "GROVE_ARTIFACTS:";
pub const GROVE_LESSONS_PREFIX: &str = "GROVE_LESSONS:";
pub const GROVE_DECISIONS_PREFIX: &str = "GROVE_DECISIONS:";
pub const GROVE_WARNINGS_PREFIX: &str = "GROVE_WARNINGS:";
pub const GROVE_EXIT_PREFIX: &str = "GROVE_EXIT:";
pub const GROVE_CHECKPOINT_PREFIX: &str = "GROVE_CHECKPOINT:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolMarker {
    Result,
    Artifacts,
    Lessons,
    Decisions,
    Warnings,
    Exit,
    Checkpoint,
}

impl ProtocolMarker {
    #[must_use]
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Result => GROVE_RESULT_PREFIX,
            Self::Artifacts => GROVE_ARTIFACTS_PREFIX,
            Self::Lessons => GROVE_LESSONS_PREFIX,
            Self::Decisions => GROVE_DECISIONS_PREFIX,
            Self::Warnings => GROVE_WARNINGS_PREFIX,
            Self::Exit => GROVE_EXIT_PREFIX,
            Self::Checkpoint => GROVE_CHECKPOINT_PREFIX,
        }
    }

    #[must_use]
    pub fn from_trimmed_line(line: &str) -> Option<Self> {
        Self::all()
            .into_iter()
            .find(|marker| line.starts_with(marker.prefix()))
    }

    #[must_use]
    pub fn all() -> [Self; 7] {
        [
            Self::Result,
            Self::Artifacts,
            Self::Lessons,
            Self::Decisions,
            Self::Warnings,
            Self::Exit,
            Self::Checkpoint,
        ]
    }
}

#[derive(Debug, Error)]
pub enum ProtocolParseError {
    #[error("unknown GROVE marker in line: {line}")]
    UnknownMarker { line: String },
    #[error("result summary cannot be empty")]
    EmptyResult,
    #[error("invalid GROVE_EXIT value `{value}`; expected true or false")]
    InvalidExitValue { value: String },
    #[error("invalid JSON array for {marker}: {source}")]
    InvalidListJson {
        marker: &'static str,
        source: serde_json::Error,
    },
    #[error("{marker} JSON array item at index {index} is not a string")]
    ListItemNotString { marker: &'static str, index: usize },
    #[error("invalid checkpoint JSON: {source}")]
    InvalidCheckpointJson { source: serde_json::Error },
    #[error("checkpoint payload must be a JSON object")]
    CheckpointNotObject,
}

pub fn parse_protocol_event(line: &str) -> Result<Option<ProtocolEvent>, ProtocolParseError> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || !trimmed.starts_with("GROVE_") {
        return Ok(None);
    }

    let Some(marker) = ProtocolMarker::from_trimmed_line(trimmed) else {
        return Err(ProtocolParseError::UnknownMarker {
            line: trimmed.to_owned(),
        });
    };

    let payload = trimmed[marker.prefix().len()..].trim();
    let event = match marker {
        ProtocolMarker::Result => ProtocolEvent::Result {
            summary: parse_result_payload(payload)?,
        },
        ProtocolMarker::Artifacts => ProtocolEvent::Artifacts {
            items: parse_list_payload(payload, GROVE_ARTIFACTS_PREFIX)?,
        },
        ProtocolMarker::Lessons => ProtocolEvent::Lessons {
            items: parse_list_payload(payload, GROVE_LESSONS_PREFIX)?,
        },
        ProtocolMarker::Decisions => ProtocolEvent::Decisions {
            items: parse_list_payload(payload, GROVE_DECISIONS_PREFIX)?,
        },
        ProtocolMarker::Warnings => ProtocolEvent::Warnings {
            items: parse_list_payload(payload, GROVE_WARNINGS_PREFIX)?,
        },
        ProtocolMarker::Exit => ProtocolEvent::Exit {
            value: parse_exit_payload(payload)?,
        },
        ProtocolMarker::Checkpoint => ProtocolEvent::Checkpoint {
            payload: parse_checkpoint_payload(payload)?,
        },
    };

    Ok(Some(event))
}

fn parse_result_payload(payload: &str) -> Result<String, ProtocolParseError> {
    let summary = payload.trim();
    if summary.is_empty() {
        Err(ProtocolParseError::EmptyResult)
    } else {
        Ok(summary.to_owned())
    }
}

fn parse_exit_payload(payload: &str) -> Result<bool, ProtocolParseError> {
    let value = payload.trim();
    if value.eq_ignore_ascii_case("true") {
        Ok(true)
    } else if value.eq_ignore_ascii_case("false") {
        Ok(false)
    } else {
        Err(ProtocolParseError::InvalidExitValue {
            value: value.to_owned(),
        })
    }
}

fn parse_list_payload(
    payload: &str,
    marker: &'static str,
) -> Result<Vec<String>, ProtocolParseError> {
    let trimmed = payload.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }

    if trimmed.starts_with('[') {
        let value: Value = serde_json::from_str(trimmed)
            .map_err(|source| ProtocolParseError::InvalidListJson { marker, source })?;
        let items = value
            .as_array()
            .ok_or_else(|| ProtocolParseError::InvalidListJson {
                marker,
                source: serde_json::Error::io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "expected array",
                )),
            })?;
        let mut parsed = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let Some(item) = item.as_str() else {
                return Err(ProtocolParseError::ListItemNotString { marker, index });
            };
            push_unique(&mut parsed, item.trim());
        }
        return Ok(parsed);
    }

    let mut parsed = Vec::new();
    for item in trimmed.split(',') {
        push_unique(&mut parsed, item.trim());
    }
    Ok(parsed)
}

fn parse_checkpoint_payload(payload: &str) -> Result<CheckpointPayload, ProtocolParseError> {
    let value: Value = serde_json::from_str(payload)
        .map_err(|source| ProtocolParseError::InvalidCheckpointJson { source })?;
    if !value.is_object() {
        return Err(ProtocolParseError::CheckpointNotObject);
    }
    serde_json::from_value(value)
        .map_err(|source| ProtocolParseError::InvalidCheckpointJson { source })
}

fn push_unique(items: &mut Vec<String>, candidate: &str) {
    if candidate.is_empty() || candidate.eq_ignore_ascii_case("none") {
        return;
    }
    if !items.iter().any(|item| item == candidate) {
        items.push(candidate.to_owned());
    }
}

#[cfg(test)]
mod tests;
