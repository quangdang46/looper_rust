use crate::schema::{
    BrCapability, BrDependencySnapshot, BrIssueDetail, BrIssueSummary, BrVersion, ShowParseError,
    parse_dep_list_output, parse_list_output, parse_ready_output, parse_show_output,
};
use grove_types::{BeadId, HandoffRecord};
use std::{
    fmt,
    path::{Path, PathBuf},
    process::Command,
};

fn bullet_list(items: &[String]) -> String {
    items
        .iter()
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_handoff_comment(handoff: &HandoffRecord) -> Option<String> {
    let mut sections = Vec::new();

    if !handoff.summary.is_empty() {
        sections.push(format!("**Summary:** {}", handoff.summary));
    }
    if !handoff.artifacts.is_empty() {
        sections.push(format!(
            "**Artifacts:**\n{}",
            bullet_list(&handoff.artifacts)
        ));
    }
    if !handoff.lessons.is_empty() {
        sections.push(format!("**Lessons:**\n{}", bullet_list(&handoff.lessons)));
    }
    if !handoff.decisions.is_empty() {
        sections.push(format!(
            "**Decisions:**\n{}",
            bullet_list(&handoff.decisions)
        ));
    }
    if !handoff.warnings.is_empty() {
        sections.push(format!("**Warnings:**\n{}", bullet_list(&handoff.warnings)));
    }

    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

pub trait BrClient {
    fn ready(&self) -> Result<Vec<BrIssueSummary>, BrError>;
    fn list_open(&self) -> Result<Vec<BrIssueSummary>, BrError>;
    fn show(&self, id: &BeadId) -> Result<BrIssueDetail, BrError>;
    fn dep_list(&self, id: &BeadId) -> Result<BrDependencySnapshot, BrError>;
    fn capability(&self) -> Result<BrCapability, BrError>;

    // Mirror outbox operations for grove-1j9.7.6
    fn close_bead(&self, id: &BeadId, reason: Option<&str>) -> Result<(), BrError>;
    fn add_comment(&self, id: &BeadId, text: &str) -> Result<(), BrError>;
    fn mirror_handoff(
        &self,
        id: &BeadId,
        handoff: &HandoffRecord,
        close_bead: bool,
    ) -> Result<(), BrError>;
}

#[derive(Debug, Clone)]
pub struct CliBrClient {
    br_bin: String,
    working_dir: PathBuf,
}

impl CliBrClient {
    #[must_use]
    pub fn new(br_bin: impl Into<String>, working_dir: impl Into<PathBuf>) -> Self {
        Self {
            br_bin: br_bin.into(),
            working_dir: working_dir.into(),
        }
    }

    #[must_use]
    pub fn working_dir(&self) -> &Path {
        &self.working_dir
    }

    fn run(&self, args: &[&str]) -> Result<CommandOutput, BrError> {
        let output = Command::new(&self.br_bin)
            .args(args)
            .current_dir(&self.working_dir)
            .output()
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    BrError::NotFound {
                        path: self.br_bin.clone(),
                    }
                } else {
                    BrError::Io(source)
                }
            })?;

        let stdout = String::from_utf8(output.stdout).map_err(BrError::Utf8)?;
        let stderr = String::from_utf8(output.stderr).map_err(BrError::Utf8)?;

        if output.status.success() {
            Ok(CommandOutput { stdout, stderr })
        } else {
            Err(BrError::CommandFailed {
                command: format_command(&self.br_bin, args),
                code: output.status.code(),
                stdout,
                stderr,
            })
        }
    }
}

impl BrClient for CliBrClient {
    fn ready(&self) -> Result<Vec<BrIssueSummary>, BrError> {
        let output = self.run(&["ready", "--json"])?;
        parse_ready_output(&output.stdout).map_err(|source| BrError::ParseError {
            command: format_command(&self.br_bin, &["ready", "--json"]),
            source,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn list_open(&self) -> Result<Vec<BrIssueSummary>, BrError> {
        let output = self.run(&["list", "--json"])?;
        parse_list_output(&output.stdout).map_err(|source| BrError::ParseError {
            command: format_command(&self.br_bin, &["list", "--json"]),
            source,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn show(&self, id: &BeadId) -> Result<BrIssueDetail, BrError> {
        let args = ["show", id.as_str(), "--json"];
        let output = self.run(&args)?;
        parse_show_output(&output.stdout, id).map_err(|error| match error {
            ShowParseError::NotFound(id) => BrError::BeadNotFound { id },
            ShowParseError::Cardinality { bead_id, count } => BrError::ProtocolViolation {
                command: format_command(&self.br_bin, &args),
                message: format!("expected exactly one bead record for {bead_id}, found {count}"),
                stdout: output.stdout,
                stderr: output.stderr,
            },
            ShowParseError::Serde(source) => BrError::ParseError {
                command: format_command(&self.br_bin, &args),
                source,
                stdout: output.stdout,
                stderr: output.stderr,
            },
        })
    }

    fn dep_list(&self, id: &BeadId) -> Result<BrDependencySnapshot, BrError> {
        let args = ["dep", "list", id.as_str(), "--json"];
        let output = self.run(&args)?;
        parse_dep_list_output(&output.stdout, id).map_err(|source| BrError::ParseError {
            command: format_command(&self.br_bin, &args),
            source,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn capability(&self) -> Result<BrCapability, BrError> {
        let beads_dir_exists = self.working_dir.join(".beads").exists();
        let output = self.run(&["--version"])?;
        let version_line =
            first_non_empty_line(&output.stdout).or_else(|| first_non_empty_line(&output.stderr));
        let version = version_line.as_deref().and_then(parse_version_line);
        Ok(BrCapability {
            available: true,
            version_line,
            version,
            beads_dir_exists,
        })
    }

    // Mirror outbox operations (grove-1j9.7.6)

    fn close_bead(&self, id: &BeadId, reason: Option<&str>) -> Result<(), BrError> {
        let mut args = vec!["close", id.as_str()];
        if let Some(reason_text) = reason {
            args.extend(["--reason", reason_text]);
        }
        // Use --json to ensure we get structured output
        args.push("--json");

        let _output = self.run(&args)?;
        // We don't need to parse the output, just check it succeeded
        Ok(())
    }

    fn add_comment(&self, id: &BeadId, text: &str) -> Result<(), BrError> {
        let args = ["comment", "add", id.as_str(), text, "--json"];
        let _output = self.run(&args)?;
        // Comment succeeded if we got here
        Ok(())
    }

    fn mirror_handoff(
        &self,
        id: &BeadId,
        handoff: &HandoffRecord,
        close_bead: bool,
    ) -> Result<(), BrError> {
        if let Some(comment) = build_handoff_comment(handoff) {
            self.add_comment(id, &comment)?;
        }

        if close_bead {
            let close_reason = if handoff.warnings.is_empty() {
                Some("Completed successfully")
            } else {
                Some("Completed with warnings")
            };
            self.close_bead(id, close_reason)?;
        }

        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BrError {
    #[error("br binary not found at {path}")]
    NotFound { path: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("br command failed ({command}) with exit code {code:?}")]
    CommandFailed {
        command: String,
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    #[error("failed to parse br output for {command}: {source}")]
    ParseError {
        command: String,
        source: serde_json::Error,
        stdout: String,
        stderr: String,
    },
    #[error("protocol violation for {command}: {message}")]
    ProtocolViolation {
        command: String,
        message: String,
        stdout: String,
        stderr: String,
    },
    #[error("bead {id} not found")]
    BeadNotFound { id: BeadId },
}

struct CommandOutput {
    stdout: String,
    stderr: String,
}

pub(crate) fn first_non_empty_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn parse_version_line(text: &str) -> Option<BrVersion> {
    let raw = text.trim();
    if raw.is_empty() {
        return None;
    }

    let mut parts = raw
        .split(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .filter(|segment| !segment.is_empty());
    let version_segment = parts.next()?;
    let mut numbers = version_segment.split('.');
    let major = numbers.next().and_then(|value| value.parse::<u64>().ok());
    let minor = numbers.next().and_then(|value| value.parse::<u64>().ok());
    let patch = numbers.next().and_then(|value| value.parse::<u64>().ok());

    Some(BrVersion {
        raw: raw.to_owned(),
        major,
        minor,
        patch,
    })
}

fn format_command(binary: &str, args: &[&str]) -> String {
    let joined = args.join(" ");
    if joined.is_empty() {
        binary.to_owned()
    } else {
        format!("{binary} {joined}")
    }
}

impl fmt::Display for CliBrClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} @ {}", self.br_bin, self.working_dir.display())
    }
}

#[cfg(test)]
mod tests;
