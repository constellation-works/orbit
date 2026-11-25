//! Fleet notice record shapes and query filters for the host store.
//!
//! The store keeps each notice's full document as opaque JSON beside the
//! columns its queries filter on; effective state, dedupe resolution,
//! authority, limits and report preservation belong to the calling operation
//! inside [`crate::Store::write`].

use chrono::{DateTime, Utc};

/// One stored notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeRow {
    /// Home workspace.
    pub workspace_id: String,
    pub id: String,
    /// `workspace` or `global`.
    pub scope: String,
    /// `incident`, `maintenance` or `info`.
    pub kind: String,
    pub dedupe_key: String,
    /// `operator`, `orchestrator` or `agent`.
    pub author_class: String,
    pub author_crew: Option<String>,
    pub author_task_id: Option<String>,
    pub linked_task_id: Option<String>,
    pub close_on_linked_done: bool,
    pub expires_at: DateTime<Utc>,
    pub opened_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Set once the stored notice is closed; never set by expiry.
    pub closed_at: Option<DateTime<Utc>>,
    pub revision: u32,
    /// The full notice document, as core's notice record.
    pub record_json: String,
}

/// One row of a notice's append-only post and close history. Post-rate
/// limits count these rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeEventRow {
    pub workspace_id: String,
    pub notice_id: String,
    pub at: DateTime<Utc>,
    /// `created`, `updated`, `corroborated`, `closed` or `linked_task_done`.
    pub action: String,
    pub poster_class: String,
    pub poster_crew: Option<String>,
    pub poster_task_id: Option<String>,
    /// Action-specific detail as a JSON object.
    pub detail_json: String,
}

/// Which notices [`crate::NoticeRepository::list`] selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeScopeFilter<'a> {
    /// Notices homed in this workspace only.
    Workspace(&'a str),
    /// Notices homed in this workspace, plus every `global` notice.
    WorkspaceAndGlobal(&'a str),
    /// Every notice across all workspaces.
    AllWorkspaces,
}

/// Which post events [`crate::NoticeRepository::list_poster_events`] selects
/// for rate-limit counting.
#[derive(Debug, Clone, Copy)]
pub struct NoticePosterEventFilter<'a> {
    pub since: DateTime<Utc>,
    /// Count the posts of this bound task.
    pub poster_task_id: Option<&'a str>,
    /// Count the posts orchestrators made as this crew.
    pub poster_crew: Option<&'a str>,
}
