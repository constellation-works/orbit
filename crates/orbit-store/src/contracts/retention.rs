//! Bounded retention over the host store: audit rows, terminal run state,
//! applied task commit journal rows, the text that can name an audit blob,
//! and the file's page accounting.
//!
//! Record shapes and query parameters only; the SQL is
//! `RetentionRepository`'s.

use chrono::{DateTime, Utc};

/// A resource-history row selected without a write lock. The timestamp
/// protects against a rowid reused by another sweep and a concurrent sampler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRetentionRow {
    pub row_id: i64,
    pub timestamp: String,
}

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
