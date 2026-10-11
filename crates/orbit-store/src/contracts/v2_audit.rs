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
    /// Payload `body_kind` values to match; empty applies no kind filter.
    /// A non-empty list also requires a valid JSON payload carrying a string
    /// `event_id`, the envelope rows the run projections can reconstruct, so
    /// a count with this filter equals what those projections would see.
    pub body_kinds: Vec<String>,
    /// Event ids to match; empty applies no id filter. Each id is one bound
    /// parameter, so callers chunk long lists under SQLite's parameter cap.
    pub event_ids: Vec<String>,
}

/// Filtered total and one chronological tail page from a single read snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2AuditEventTailPage {
    /// Matching rows visible to the snapshot. Limit and offset do not apply.
    pub total: i64,
    /// The tail page, oldest first.
    pub events: Vec<V2AuditEventRow>,
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
