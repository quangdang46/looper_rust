use grove_types::{BeadId, BeadPriority, BeadRef, Timestamp};
use serde::{Deserialize, Serialize, de::Error as _};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrVersion {
    pub raw: String,
    pub major: Option<u64>,
    pub minor: Option<u64>,
    pub patch: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrCapability {
    pub available: bool,
    pub version_line: Option<String>,
    pub version: Option<BrVersion>,
    pub beads_dir_exists: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrIssueSummary {
    pub id: BeadId,
    pub title: String,
    pub description: Option<String>,
    pub priority: BeadPriority,
    pub issue_type: String,
    pub status: String,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub blocked_by: Vec<BeadId>,
    pub blocks: Vec<BeadId>,
    pub raw_json: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrComment {
    pub id: String,
    pub text: String,
    pub author: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrIssueDetail {
    pub summary: BrIssueSummary,
    pub closed_at: Option<Timestamp>,
    pub close_reason: Option<String>,
    pub comments: Vec<BrComment>,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrDependencyRow {
    pub issue_id: BeadId,
    pub depends_on_id: BeadId,
    pub dependency_type: String,
    pub title: Option<String>,
    pub status: Option<String>,
    pub priority: Option<BeadPriority>,
    pub raw_json: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrDependencySnapshot {
    pub bead_id: BeadId,
    pub blocked_by: Vec<BeadId>,
    pub blocks: Vec<BeadId>,
    pub rows: Vec<BrDependencyRow>,
}

impl BrIssueSummary {
    #[must_use]
    pub fn into_bead_ref(self) -> BeadRef {
        BeadRef {
            id: self.id,
            title: self.title,
            description: self.description,
            priority: self.priority,
            issue_type: self.issue_type,
            br_status: self.status,
            assignee: self.assignee,
            labels: self.labels,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    #[must_use]
    pub fn dependency_snapshot(&self) -> BrDependencySnapshot {
        BrDependencySnapshot {
            bead_id: self.id.clone(),
            blocked_by: self.blocked_by.clone(),
            blocks: self.blocks.clone(),
            rows: Vec::new(),
        }
    }
}

pub(crate) fn parse_ready_output(text: &str) -> Result<Vec<BrIssueSummary>, serde_json::Error> {
    let payload = serde_json::from_str::<ReadyWire>(text)?;
    let issues = match payload {
        ReadyWire::Envelope { issues, count } => {
            let _ = count;
            issues
        }
        ReadyWire::List(issues) => issues,
    };
    issues.into_iter().map(BrIssueSummary::try_from).collect()
}

pub(crate) fn parse_list_output(text: &str) -> Result<Vec<BrIssueSummary>, serde_json::Error> {
    serde_json::from_str::<Vec<IssueWire>>(text)?
        .into_iter()
        .map(BrIssueSummary::try_from)
        .collect()
}

pub(crate) fn parse_show_output(
    text: &str,
    bead_id: &BeadId,
) -> Result<BrIssueDetail, ShowParseError> {
    match serde_json::from_str::<IssueWire>(text) {
        Ok(issue) => BrIssueDetail::try_from(issue).map_err(ShowParseError::Serde),
        Err(object_error) => match serde_json::from_str::<Vec<IssueWire>>(text) {
            Ok(mut issues) => {
                if issues.is_empty() {
                    Err(ShowParseError::NotFound(bead_id.clone()))
                } else if issues.len() == 1 {
                    BrIssueDetail::try_from(issues.remove(0)).map_err(ShowParseError::Serde)
                } else {
                    Err(ShowParseError::Cardinality {
                        bead_id: bead_id.clone(),
                        count: issues.len(),
                    })
                }
            }
            Err(_) => Err(ShowParseError::Serde(object_error)),
        },
    }
}

pub(crate) fn parse_dep_list_output(
    text: &str,
    bead_id: &BeadId,
) -> Result<BrDependencySnapshot, serde_json::Error> {
    match serde_json::from_str::<DepListWire>(text)? {
        DepListWire::Snapshot { blocked_by, blocks } => Ok(BrDependencySnapshot {
            bead_id: bead_id.clone(),
            blocked_by: blocked_by.into_iter().map(BeadId::new).collect(),
            blocks: blocks.into_iter().map(BeadId::new).collect(),
            rows: Vec::new(),
        }),
        DepListWire::Rows(rows) => {
            let normalized_rows: Vec<BrDependencyRow> = rows
                .into_iter()
                .map(BrDependencyRow::try_from)
                .collect::<Result<_, _>>()?;
            let blocked_by = normalized_rows
                .iter()
                .filter(|row| row.issue_id == *bead_id)
                .map(|row| row.depends_on_id.clone())
                .collect();
            let blocks = normalized_rows
                .iter()
                .filter(|row| row.depends_on_id == *bead_id)
                .map(|row| row.issue_id.clone())
                .collect();
            Ok(BrDependencySnapshot {
                bead_id: bead_id.clone(),
                blocked_by,
                blocks,
                rows: normalized_rows,
            })
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ShowParseError {
    #[error("bead {0} not found")]
    NotFound(BeadId),
    #[error("expected exactly one bead record for {bead_id}, found {count}")]
    Cardinality { bead_id: BeadId, count: usize },
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ReadyWire {
    Envelope {
        issues: Vec<IssueWire>,
        count: usize,
    },
    List(Vec<IssueWire>),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DepListWire {
    Snapshot {
        blocked_by: Vec<String>,
        blocks: Vec<String>,
    },
    Rows(Vec<DepRowWire>),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct IssueWire {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    priority: i32,
    issue_type: String,
    status: String,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    blocked_by: Vec<String>,
    #[serde(default)]
    blocks: Vec<String>,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    close_reason: Option<String>,
    #[serde(default)]
    comments: Vec<CommentWire>,
    #[serde(default)]
    metadata: Option<Value>,
    #[serde(default)]
    dependencies: Vec<RelatedIssueWire>,
    #[serde(default)]
    dependents: Vec<RelatedIssueWire>,
    #[serde(flatten)]
    extra: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RelatedIssueWire {
    id: String,
    #[serde(default)]
    dependency_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CommentWire {
    id: Value,
    text: String,
    #[serde(default)]
    author: Option<String>,
    created_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct DepRowWire {
    issue_id: String,
    depends_on_id: String,
    #[serde(rename = "type")]
    dependency_type: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<i32>,
    #[serde(flatten)]
    raw_json: Value,
}

impl TryFrom<IssueWire> for BrIssueSummary {
    type Error = serde_json::Error;

    fn try_from(value: IssueWire) -> Result<Self, Self::Error> {
        let metadata = serde_json::to_value(&value)?;
        let blocked_by = if value.blocked_by.is_empty() {
            value
                .dependencies
                .iter()
                .map(|item| BeadId::new(item.id.clone()))
                .collect()
        } else {
            value.blocked_by.iter().cloned().map(BeadId::new).collect()
        };
        let blocks = if value.blocks.is_empty() {
            value
                .dependents
                .iter()
                .map(|item| BeadId::new(item.id.clone()))
                .collect()
        } else {
            value.blocks.iter().cloned().map(BeadId::new).collect()
        };

        Ok(Self {
            id: BeadId::new(value.id),
            title: value.title,
            description: value.description,
            priority: parse_priority(value.priority).map_err(serde_json::Error::custom)?,
            issue_type: value.issue_type,
            status: value.status,
            assignee: value.assignee,
            labels: value.labels,
            created_at: parse_timestamp(&value.created_at).map_err(serde_json::Error::custom)?,
            updated_at: parse_timestamp(&value.updated_at).map_err(serde_json::Error::custom)?,
            blocked_by,
            blocks,
            raw_json: metadata,
        })
    }
}

impl TryFrom<IssueWire> for BrIssueDetail {
    type Error = serde_json::Error;

    fn try_from(value: IssueWire) -> Result<Self, Self::Error> {
        let summary = BrIssueSummary::try_from(value.clone())?;
        let closed_at = value
            .closed_at
            .as_deref()
            .map(parse_timestamp)
            .transpose()
            .map_err(serde_json::Error::custom)?;
        let comments = value
            .comments
            .into_iter()
            .map(BrComment::try_from)
            .collect::<Result<_, _>>()?;

        Ok(Self {
            summary,
            closed_at,
            close_reason: value.close_reason,
            comments,
            metadata: value.metadata.unwrap_or_else(|| value.extra.clone()),
        })
    }
}

impl TryFrom<CommentWire> for BrComment {
    type Error = serde_json::Error;

    fn try_from(value: CommentWire) -> Result<Self, Self::Error> {
        Ok(Self {
            id: match value.id {
                Value::String(text) => text,
                Value::Number(number) => number.to_string(),
                other => {
                    return Err(serde_json::Error::custom(format!(
                        "unsupported comment id: {other}"
                    )));
                }
            },
            text: value.text,
            author: value.author,
            created_at: parse_timestamp(&value.created_at).map_err(serde_json::Error::custom)?,
        })
    }
}

impl TryFrom<DepRowWire> for BrDependencyRow {
    type Error = serde_json::Error;

    fn try_from(value: DepRowWire) -> Result<Self, Self::Error> {
        let raw_json = serde_json::to_value(&value)?;
        Ok(Self {
            issue_id: BeadId::new(value.issue_id),
            depends_on_id: BeadId::new(value.depends_on_id),
            dependency_type: value.dependency_type,
            title: value.title,
            status: value.status,
            priority: value
                .priority
                .map(parse_priority)
                .transpose()
                .map_err(serde_json::Error::custom)?,
            raw_json,
        })
    }
}

fn parse_timestamp(input: &str) -> Result<Timestamp, String> {
    chrono::DateTime::parse_from_rfc3339(input)
        .map(|ts| ts.with_timezone(&chrono::Utc))
        .map_err(|err| format!("invalid timestamp `{input}`: {err}"))
}

fn parse_priority(value: i32) -> Result<BeadPriority, String> {
    match value {
        0 => Ok(BeadPriority::P0),
        1 => Ok(BeadPriority::P1),
        2 => Ok(BeadPriority::P2),
        3 => Ok(BeadPriority::P3),
        4 => Ok(BeadPriority::P4),
        other => Err(format!("unsupported bead priority `{other}`")),
    }
}

#[cfg(test)]
mod tests;
