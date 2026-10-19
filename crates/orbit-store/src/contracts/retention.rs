//! Bounded retention over the host store: audit rows, terminal run state,
//! applied task commit journal rows, the text that can name an audit blob,
//! and the file's page accounting.
//!
//! Every mutation is one short statement or transaction over at most `limit`
//! rows, so a caller sweeping a large history never holds the write lock for
//! longer than one batch.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;

/// Minimal lock-contention evidence retained independently of command audit.
#[derive(Debug, Clone)]
pub struct TaskLockDenialRecord {
    /// Original invocation identity, used when no task or run was named.
    pub execution_id: String,
    /// Time of the refused request.
    pub timestamp: DateTime<Utc>,
    /// Owning run when the request did not name tasks.
    pub job_run_id: Option<String>,
    /// Requester identities and conflicting paths, without command payloads.
    pub arguments_json: String,
}

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

/// Which workspace partitions one retention pass may touch.
///
/// `legacy_ids` is resolved once by the caller. `None` keeps the pass on
/// `workspace_id` alone, which is what a caller uses when it cannot confirm
/// the set of registered workspaces. `Some` also selects those ids. Other
/// partitions are never implied: a registered workspace keeps its own window,
/// including rows older than the cutoff.
#[derive(Debug, Clone, Copy)]
pub struct RetentionWorkspaceScope<'a> {
    /// The workspace this command is running for. Always selected.
    pub workspace_id: &'a str,
    /// Extra partitions this pass may select, already limited to ids the
    /// caller has decided are not registered.
    pub legacy_ids: Option<&'a [String]>,
}

/// Tables whose leading `workspace_id` column a retention pass enumerates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionIdSource {
    /// `v2_audit_events`.
    RunAudit,
    /// `job_runs`.
    JobRuns,
    /// `task_commit_journal`.
    CommitJournal,
}

/// Applied task commit journal rows a retention pass may delete, named by
/// primary key so each delete batch is short and repeats no scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalRetentionCandidates {
    /// Oldest settled first.
    pub journal_ids: Vec<String>,
    /// Bytes of the selected rows' stored text.
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
    /// All lock-conflict attempts in one workspace, independent of audit retention.
    fn list_task_lock_denials(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<TaskLockDenialRecord>, OrbitError>;

    /// Distinct `workspace_id` values in `source`, read from the index that
    /// leads with that column. The caller decides which of them are in scope.
    fn distinct_workspace_ids(&self, source: RetentionIdSource) -> Result<Vec<String>, OrbitError>;

    /// Rows of `table` older than `cutoff`. A run-audit selection covers
    /// `scope`; the command audit is host-wide and ignores `scope`.
    fn audit_retention_selection(
        &self,
        table: AuditRetentionTable,
        scope: RetentionWorkspaceScope<'_>,
        cutoff: DateTime<Utc>,
    ) -> Result<RetentionSelection, OrbitError>;

    /// Delete at most `limit` of the rows [`Self::audit_retention_selection`]
    /// selects, in one transaction. Returns how many went.
    fn prune_audit_retention_batch(
        &self,
        table: AuditRetentionTable,
        scope: RetentionWorkspaceScope<'_>,
        cutoff: DateTime<Utc>,
        limit: usize,
    ) -> Result<usize, OrbitError>;

    /// Pipeline state of terminal runs in `scope` that finished before
    /// `cutoff`. A held run still awaits review evidence that resumes it, so
    /// it is never selected; neither is a run that is not terminal.
    fn run_state_retention_selection(
        &self,
        scope: RetentionWorkspaceScope<'_>,
        cutoff: DateTime<Utc>,
    ) -> Result<RetentionSelection, OrbitError>;

    /// Drop the pipeline state of at most `limit` selected runs and stamp
    /// their `archived_at`, in one transaction. The run row and its steps stay.
    /// Each row is deleted under its own `workspace_id`.
    fn archive_run_states_batch(
        &self,
        scope: RetentionWorkspaceScope<'_>,
        cutoff: DateTime<Utc>,
        archived_at: DateTime<Utc>,
        limit: usize,
    ) -> Result<usize, OrbitError>;

    /// `applied` task commit journal rows in `scope` settled before `cutoff`,
    /// except each task's newest applied row. Recovery replays only
    /// `prepared` and `committed` rows, so neither is ever selected, and
    /// `aborted` rows are left alone. One read; it takes no write lock.
    fn journal_retention_candidates(
        &self,
        scope: RetentionWorkspaceScope<'_>,
        cutoff: DateTime<Utc>,
    ) -> Result<JournalRetentionCandidates, OrbitError>;

    /// Delete the named journal rows that are still `applied`, in one
    /// transaction. Returns how many went.
    fn delete_applied_journal_rows(&self, journal_ids: &[String]) -> Result<usize, OrbitError>;

    /// Call `visit` with every stored text that can name an audit blob: run
    /// audit payloads, step responses and errors, and pipeline state. Run-audit
    /// rows of `excluding` older than the cutoff are skipped, so a plan sees
    /// the references a prune would leave. Reads in short chunks, never one
    /// long read transaction.
    fn visit_blob_reference_text(
        &self,
        excluding: Option<(RetentionWorkspaceScope<'_>, DateTime<Utc>)>,
        visit: &mut dyn FnMut(&str),
    ) -> Result<(), OrbitError>;

    fn store_space(&self) -> Result<StoreSpace, OrbitError>;
}
