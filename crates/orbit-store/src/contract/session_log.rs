use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLogKind {
    Status,
    Note,
    CheckLater,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionLogEntry {
    pub id: String,
    pub at: DateTime<Utc>,
    pub kind: SessionLogKind,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_task_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_run_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLogAppendParams {
    pub kind: SessionLogKind,
    pub body: String,
    pub related_task_ids: Vec<String>,
    pub related_run_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionLogFilter {
    pub kind: Option<SessionLogKind>,
    pub unresolved_only: bool,
    pub since: Option<DateTime<Utc>>,
}
