//! Fleet notice records in the owner store. The store keeps each notice's
//! full document as opaque JSON beside the columns its reads and the posting
//! transaction filter on; effective state, dedupe resolution, authority and
//! limits are core's, decided inside [`NoticeStoreBackend::post_notice`]'s
//! transaction so one open notice per key holds under concurrent posts.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;

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

/// What a post's decision reads inside the posting transaction.
#[derive(Debug, Clone, Default)]
pub struct NoticePostSnapshot {
    /// Every stored-open notice of the home workspace, and every stored-open
    /// `global` notice in this store, whatever its home.
    pub unclosed: Vec<NoticeRow>,
    /// Post events since the query's `events_since` by the poster's bound
    /// task, or by orchestrators posting as the poster's crew.
    pub poster_events: Vec<NoticeEventRow>,
}

/// The rows a post's snapshot selects, and the retention purge it runs.
#[derive(Debug, Clone)]
pub struct NoticePostQuery<'a> {
    pub workspace_id: &'a str,
    /// Count the posts of this bound task.
    pub poster_task_id: Option<&'a str>,
    /// Count the posts orchestrators made as this crew.
    pub poster_crew: Option<&'a str>,
    pub events_since: DateTime<Utc>,
    /// Delete the home workspace's notices that stopped being in force
    /// (closed, or expired) before this instant, with their history.
    pub purge_before: Option<DateTime<Utc>>,
}

/// The write a post's decision chose.
#[derive(Debug, Clone)]
pub enum NoticePostPlan {
    /// A new notice.
    Insert {
        row: NoticeRow,
        event: NoticeEventRow,
    },
    /// Replace an open notice computed from the snapshot at
    /// `expected_revision`.
    Update {
        row: NoticeRow,
        expected_revision: u32,
        event: NoticeEventRow,
    },
}

/// Core's post decision, run inside the posting transaction. An error
/// writes nothing; the retention purge rolls back with it.
pub type NoticePostDecision<'a> =
    dyn Fn(&NoticePostSnapshot) -> Result<NoticePostPlan, OrbitError> + 'a;

pub trait NoticeStoreBackend: Send + Sync {
    /// Read the snapshot, run `decide` and apply its plan in one immediate
    /// transaction. Returns the written row.
    fn post_notice(
        &self,
        query: &NoticePostQuery<'_>,
        decide: &NoticePostDecision<'_>,
    ) -> Result<NoticeRow, OrbitError>;

    /// Store `row` as closed unless the stored notice is already closed or
    /// no longer at `expected_revision`. Returns whether it was written.
    fn close_notice(
        &self,
        row: &NoticeRow,
        expected_revision: u32,
        event: &NoticeEventRow,
    ) -> Result<bool, OrbitError>;

    /// One notice by id.
    fn notice_row(&self, id: &str) -> Result<Option<NoticeRow>, OrbitError>;

    /// Notices homed in `workspace_ids` (every workspace when `None`), plus
    /// every `global` notice when `include_global`, newest update first.
    fn notice_rows(
        &self,
        workspace_ids: Option<&[String]>,
        include_global: bool,
    ) -> Result<Vec<NoticeRow>, OrbitError>;

    /// Stored-open notices linked to `task_id`.
    fn notices_linked_to(&self, task_id: &str) -> Result<Vec<NoticeRow>, OrbitError>;

    /// One notice's history, oldest first.
    fn notice_events(&self, notice_id: &str) -> Result<Vec<NoticeEventRow>, OrbitError>;
}
