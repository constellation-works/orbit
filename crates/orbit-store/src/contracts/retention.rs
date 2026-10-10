//! Bounded retention over the host store: audit rows, terminal run state,
//! the text that can name an audit blob, and the file's page accounting.
//!
//! Every mutation is one short statement or transaction over at most `limit`
//! rows, so a caller sweeping a large history never holds the write lock for
//! longer than one batch.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;

/// The two audit tables retention prunes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditRetentionTable {
    /// `audit_events`: the host-wide command audit, keyed by `timestamp`.
    Command,
    /// `v2_audit_events`: one workspace's run envelope and loop events,
    /// keyed by `ts`.
    Run,
}

impl AuditRetentionTable {
    /// The SQLite table this names.
    pub const fn table_name(self) -> &'static str {
        match self {
            Self::Command => "audit_events",
            Self::Run => "v2_audit_events",
        }
    }
}

/// Rows a cutoff selects and the bytes of their stored text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionSelection {
    pub rows: u64,
    pub bytes: u64,
}

/// The database file's page accounting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreSpace {
    pub page_size: u64,
    pub page_count: u64,
    /// Pages a delete freed for reuse. Only `VACUUM` returns them to the
    /// filesystem.
    pub freelist_pages: u64,
}

pub trait StoreRetentionBackend: Send + Sync {
    /// Rows of `table` older than `cutoff`. A run-audit selection is scoped to
    /// `workspace_id`; the command audit is host-wide.
    fn audit_retention_selection(
        &self,
        table: AuditRetentionTable,
        workspace_id: &str,
        cutoff: DateTime<Utc>,
    ) -> Result<RetentionSelection, OrbitError>;

    /// Delete at most `limit` of the rows [`Self::audit_retention_selection`]
    /// selects, in one statement. Returns how many went.
    fn prune_audit_retention_batch(
        &self,
        table: AuditRetentionTable,
        workspace_id: &str,
        cutoff: DateTime<Utc>,
        limit: usize,
    ) -> Result<usize, OrbitError>;

    /// The pipeline state kept by this workspace's terminal runs that
    /// finished before `cutoff`. A held run still awaits review evidence that
    /// resumes it, so it is never selected; neither is a run that is not
    /// terminal.
    fn run_state_retention_selection(
        &self,
        workspace_id: &str,
        cutoff: DateTime<Utc>,
    ) -> Result<RetentionSelection, OrbitError>;

    /// Drop the pipeline state of at most `limit` selected runs and stamp
    /// their `archived_at`, in one transaction. The run row and its steps stay.
    fn archive_run_states_batch(
        &self,
        workspace_id: &str,
        cutoff: DateTime<Utc>,
        archived_at: DateTime<Utc>,
        limit: usize,
    ) -> Result<usize, OrbitError>;

    /// Call `visit` with every stored text that can name an audit blob: run
    /// audit payloads, step responses and errors, and pipeline state. Rows of
    /// `excluding` (a workspace and a cutoff) older than the cutoff are
    /// skipped, so a plan sees the references a prune would leave. Reads in
    /// short chunks, never one long read transaction.
    fn visit_blob_reference_text(
        &self,
        excluding: Option<(&str, DateTime<Utc>)>,
        visit: &mut dyn FnMut(&str),
    ) -> Result<(), OrbitError>;

    fn store_space(&self) -> Result<StoreSpace, OrbitError>;
}
