#![allow(clippy::unwrap_used, clippy::expect_used)]
use grove_types::{ProtocolEvent, SessionId, Timestamp, TranscriptEvent};
use std::{
    convert::TryFrom,
    fs::{File, OpenOptions, create_dir_all},
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TranscriptError {
    #[error("failed to create transcript directory {path}: {source}")]
    CreateDir {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to open transcript file {path}: {source}")]
    OpenFile {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to read transcript file {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to decode transcript line {line} from {path}: {source}")]
    DecodeLine {
        path: String,
        line: usize,
        source: serde_json::Error,
    },
    #[error("invalid transcript line {line} in {path}: {reason}")]
    InvalidLine {
        path: String,
        line: usize,
        reason: String,
    },
    #[error("failed to encode transcript event: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("failed to write transcript file {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to flush transcript file {path}: {source}")]
    Flush {
        path: String,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone)]
pub struct TranscriptReplay {
    pub events: Vec<TranscriptEvent>,
}

pub struct TranscriptWriter {
    path: String,
    writer: BufWriter<File>,
}

impl TranscriptWriter {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TranscriptError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            create_dir_all(parent).map_err(|source| TranscriptError::CreateDir {
                path: parent.display().to_string(),
                source,
            })?;
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|source| TranscriptError::OpenFile {
                path: path.display().to_string(),
                source,
            })?;

        Ok(Self {
            path: path.display().to_string(),
            writer: BufWriter::new(file),
        })
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn append_event(&mut self, event: &TranscriptEvent) -> Result<(), TranscriptError> {
        serde_json::to_writer(
            &mut self.writer,
            &SerializableTranscriptEvent::from(event.clone()),
        )?;
        self.writer
            .write_all(b"\n")
            .map_err(|source| TranscriptError::Write {
                path: self.path.clone(),
                source,
            })?;
        self.writer
            .flush()
            .map_err(|source| TranscriptError::Flush {
                path: self.path.clone(),
                source,
            })
    }

    pub fn append_session_started(
        &mut self,
        session_id: SessionId,
        ts: Timestamp,
    ) -> Result<(), TranscriptError> {
        self.append_event(&TranscriptEvent::SessionStarted { session_id, ts })
    }

    pub fn append_stdout_line(
        &mut self,
        line: impl Into<String>,
        ts: Timestamp,
    ) -> Result<(), TranscriptError> {
        self.append_event(&TranscriptEvent::StdoutLine {
            line: line.into(),
            ts,
        })
    }

    pub fn append_stderr_line(
        &mut self,
        line: impl Into<String>,
        ts: Timestamp,
    ) -> Result<(), TranscriptError> {
        self.append_event(&TranscriptEvent::StderrLine {
            line: line.into(),
            ts,
        })
    }

    pub fn append_protocol_event(
        &mut self,
        event: ProtocolEvent,
        ts: Timestamp,
    ) -> Result<(), TranscriptError> {
        self.append_event(&TranscriptEvent::ParsedProtocol { event, ts })
    }

    pub fn append_session_ended(
        &mut self,
        exit_code: Option<i32>,
        ts: Timestamp,
    ) -> Result<(), TranscriptError> {
        self.append_event(&TranscriptEvent::SessionEnded { exit_code, ts })
    }
}

#[derive(serde::Serialize)]
struct SerializableTranscriptEvent {
    ts: Timestamp,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event: Option<SerializableProtocolEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
}

impl From<TranscriptEvent> for SerializableTranscriptEvent {
    fn from(event: TranscriptEvent) -> Self {
        match event {
            TranscriptEvent::SessionStarted { session_id, ts } => Self {
                ts,
                kind: "session_started",
                session_id: Some(session_id.to_string()),
                line: None,
                event: None,
                exit_code: None,
            },
            TranscriptEvent::StdoutLine { line, ts } => Self {
                ts,
                kind: "stdout",
                session_id: None,
                line: Some(line),
                event: None,
                exit_code: None,
            },
            TranscriptEvent::StderrLine { line, ts } => Self {
                ts,
                kind: "stderr",
                session_id: None,
                line: Some(line),
                event: None,
                exit_code: None,
            },
            TranscriptEvent::ParsedProtocol { event, ts } => Self {
                ts,
                kind: "protocol",
                session_id: None,
                line: None,
                event: Some(event.into()),
                exit_code: None,
            },
            TranscriptEvent::SessionEnded { exit_code, ts } => Self {
                ts,
                kind: "session_ended",
                session_id: None,
                line: None,
                event: None,
                exit_code,
            },
        }
    }
}

#[derive(serde::Serialize)]
#[serde(tag = "type")]
enum SerializableProtocolEvent {
    #[serde(rename = "result")]
    Result { summary: String },
    #[serde(rename = "artifacts")]
    Artifacts { items: Vec<String> },
    #[serde(rename = "lessons")]
    Lessons { items: Vec<String> },
    #[serde(rename = "decision")]
    Decisions { items: Vec<String> },
    #[serde(rename = "warning")]
    Warnings { items: Vec<String> },
    #[serde(rename = "exit")]
    Exit { value: bool },
    #[serde(rename = "checkpoint")]
    Checkpoint {
        payload: grove_types::CheckpointPayload,
    },
}

impl From<ProtocolEvent> for SerializableProtocolEvent {
    fn from(event: ProtocolEvent) -> Self {
        match event {
            ProtocolEvent::Result { summary } => Self::Result { summary },
            ProtocolEvent::Artifacts { items } => Self::Artifacts { items },
            ProtocolEvent::Lessons { items } => Self::Lessons { items },
            ProtocolEvent::Decisions { items } => Self::Decisions { items },
            ProtocolEvent::Warnings { items } => Self::Warnings { items },
            ProtocolEvent::Exit { value } => Self::Exit { value },
            ProtocolEvent::Checkpoint { payload } => Self::Checkpoint { payload },
        }
    }
}

#[derive(serde::Deserialize)]
struct DeserializableTranscriptEvent {
    ts: Timestamp,
    kind: String,
    session_id: Option<String>,
    line: Option<String>,
    event: Option<DeserializableProtocolEvent>,
    exit_code: Option<i32>,
}

impl TryFrom<DeserializableTranscriptEvent> for TranscriptEvent {
    type Error = String;

    fn try_from(value: DeserializableTranscriptEvent) -> Result<Self, Self::Error> {
        match value.kind.as_str() {
            "session_started" => {
                let session_id = value
                    .session_id
                    .ok_or_else(|| "session_started is missing session_id".to_owned())?;
                Ok(Self::SessionStarted {
                    session_id: SessionId::new(session_id),
                    ts: value.ts,
                })
            }
            "stdout" => Ok(Self::StdoutLine {
                line: value
                    .line
                    .ok_or_else(|| "stdout is missing line".to_owned())?,
                ts: value.ts,
            }),
            "stderr" => Ok(Self::StderrLine {
                line: value
                    .line
                    .ok_or_else(|| "stderr is missing line".to_owned())?,
                ts: value.ts,
            }),
            "protocol" => Ok(Self::ParsedProtocol {
                event: value
                    .event
                    .ok_or_else(|| "protocol is missing event".to_owned())?
                    .into_protocol_event()?,
                ts: value.ts,
            }),
            "session_ended" => Ok(Self::SessionEnded {
                exit_code: value.exit_code,
                ts: value.ts,
            }),
            other => Err(format!("unknown transcript kind `{other}`")),
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(tag = "type")]
enum DeserializableProtocolEvent {
    #[serde(rename = "result")]
    Result { summary: String },
    #[serde(rename = "artifacts")]
    Artifacts { items: Vec<String> },
    #[serde(rename = "lessons")]
    Lessons { items: Vec<String> },
    #[serde(rename = "decision")]
    Decisions { items: Vec<String> },
    #[serde(rename = "warning")]
    Warnings { items: Vec<String> },
    #[serde(rename = "exit")]
    Exit { value: bool },
    #[serde(rename = "checkpoint")]
    Checkpoint {
        payload: grove_types::CheckpointPayload,
    },
}

impl DeserializableProtocolEvent {
    fn into_protocol_event(self) -> Result<ProtocolEvent, String> {
        Ok(match self {
            Self::Result { summary } => ProtocolEvent::Result { summary },
            Self::Artifacts { items } => ProtocolEvent::Artifacts { items },
            Self::Lessons { items } => ProtocolEvent::Lessons { items },
            Self::Decisions { items } => ProtocolEvent::Decisions { items },
            Self::Warnings { items } => ProtocolEvent::Warnings { items },
            Self::Exit { value } => ProtocolEvent::Exit { value },
            Self::Checkpoint { payload } => ProtocolEvent::Checkpoint { payload },
        })
    }
}

pub fn replay_transcript(path: impl AsRef<Path>) -> Result<TranscriptReplay, TranscriptError> {
    let path = path.as_ref();
    let path_string = path.display().to_string();
    let file = File::open(path).map_err(|source| TranscriptError::OpenFile {
        path: path_string.clone(),
        source,
    })?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();

    for (index, line) in reader.lines().enumerate() {
        let line_no = index + 1;
        let line = line.map_err(|source| TranscriptError::Read {
            path: path_string.clone(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }

        let decoded: DeserializableTranscriptEvent =
            serde_json::from_str(&line).map_err(|source| TranscriptError::DecodeLine {
                path: path_string.clone(),
                line: line_no,
                source,
            })?;
        let event =
            TranscriptEvent::try_from(decoded).map_err(|reason| TranscriptError::InvalidLine {
                path: path_string.clone(),
                line: line_no,
                reason,
            })?;
        events.push(event);
    }

    Ok(TranscriptReplay { events })
}

#[cfg(test)]
mod tests;
