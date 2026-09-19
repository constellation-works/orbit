//! Contracts for the durable task/reservation commit boundary.
//!
//! One task transition, its history, a file reservation, and the dependent
//! coordination rows an admission decision needs are published as a single
//! durable outcome. The boundary itself lives in
//! `repository::task::coordination`; these are the caller-visible parameter
//! and result shapes, free of any persistence technology.

use orbit_types::task::{TaskHistoryEntry, TaskStatus};
use serde::{Deserialize, Serialize};

use crate::contracts::{
    ExpiredTaskReservation, TaskLockConflict, TaskReservationReserveParams,
    TaskReservationReserveResult,
};

/// One dependent coordination row published with a task transition.
///
/// `kind` names the caller's row family (an admission receipt, a claim, a
/// tombstone); `(kind, row_id)` is unique per workspace, so replaying a commit
/// with the same identity is refused rather than duplicated. `payload_json` is
/// opaque here: the boundary stores and returns it without interpreting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCoordinationRow {
    pub kind: String,
    pub row_id: String,
    pub payload_json: String,
}

/// One atomic publication: a task transition plus history, an optional file
/// reservation, and optional dependent coordination rows.
#[derive(Debug, Clone, Default)]
pub struct TaskCoordinationCommitParams {
    pub task_id: String,
    pub actor: String,
    /// Compare-and-set evaluated inside the boundary. Empty accepts any
    /// current status; a non-empty set that does not contain the task's
    /// current status yields [`TaskCoordinationCommitOutcome::Stale`] without
    /// writing anything.
    pub expected_status: Vec<TaskStatus>,
    /// Target status. `None` keeps the current status.
    pub status: Option<TaskStatus>,
    /// Event type recorded for the transition. Defaults to `status_changed`
    /// when the status actually moves.
    pub status_event: Option<String>,
    pub status_note: Option<String>,
    pub append_history: Vec<TaskHistoryEntry>,
    pub reservation: Option<TaskReservationReserveParams>,
    pub rows: Vec<TaskCoordinationRow>,
}

/// What a commit published, or why it published nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskCoordinationCommitOutcome {
    Committed(TaskCoordinationCommit),
    /// The task's current status is outside `expected_status`. Nothing was
    /// written; the caller re-reads and decides again.
    Stale {
        current_status: TaskStatus,
    },
    /// Requested reservation files overlap an active reservation. Nothing was
    /// written.
    Conflicted {
        conflicts: Vec<TaskLockConflict>,
        expired_reservations: Vec<ExpiredTaskReservation>,
    },
    /// A dependent coordination row with this identity already exists.
    /// Nothing was written; the caller replays its own stored outcome.
    RowExists {
        kind: String,
        row_id: String,
    },
}

/// The published outcome of one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCoordinationCommit {
    /// Identity of the durable commit decision. Recovery is keyed by it.
    pub journal_id: String,
    pub task_id: String,
    pub status: TaskStatus,
    pub reservation: Option<TaskReservationReserveResult>,
    pub rows: Vec<TaskCoordinationRow>,
}

/// Lifecycle of one durable commit decision.
///
/// `Prepared` is not yet decided: recovery rolls it back. `Committed` is
/// decided and durable: recovery rolls it forward onto the task bundle.
/// `Applied` and `Aborted` are settled and need no recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskCommitJournalState {
    Prepared,
    Committed,
    Applied,
    Aborted,
}

impl TaskCommitJournalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Committed => "committed",
            Self::Applied => "applied",
            Self::Aborted => "aborted",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "prepared" => Some(Self::Prepared),
            "committed" => Some(Self::Committed),
            "applied" => Some(Self::Applied),
            "aborted" => Some(Self::Aborted),
            _ => None,
        }
    }
}

/// One journal row as recovery reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCommitJournalRecord {
    pub journal_id: String,
    pub workspace_id: String,
    pub task_id: String,
    pub state: TaskCommitJournalState,
    /// Serialized bundle-side intent, replayed verbatim by recovery.
    pub intent_json: String,
    pub reservation_id: Option<String>,
}
