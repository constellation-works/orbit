use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct V2AuditEventInsertParams {
    pub workspace_id: String,
    pub event_id: String,
    pub source: String,
    pub schema_version: u32,
    pub event_type: String,
    pub ts: DateTime<Utc>,
    pub run_id: String,
    pub agent_identity: String,
    pub parent_event_id: Option<String>,
    pub workspace_path: Option<String>,
    pub payload_json: String,
}

#[derive(Debug, Clone, Default)]
pub struct V2AuditEventFilter {
    pub workspace_id: String,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub run_id: Option<String>,
    pub event_type: Option<String>,
    pub source: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    /// Page from the oldest event instead of the newest.
    pub oldest_first: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2AuditEventRow {
    pub id: i64,
    pub workspace_id: String,
    pub event_id: String,
    pub source: String,
    pub schema_version: u32,
    pub event_type: String,
    pub ts: DateTime<Utc>,
    pub run_id: String,
    pub agent_identity: String,
    pub parent_event_id: Option<String>,
    pub workspace_path: Option<String>,
    pub payload_json: String,
}
