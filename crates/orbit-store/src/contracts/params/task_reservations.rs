use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLockHolder {
    Task,
    Reservation,
    /// A hard path lease, named by its `lease-<id>`.
    Lease,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLockConflict {
    pub file: String,
    pub held_by: TaskLockHolder,
    pub held_by_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpiredTaskReservation {
    pub reservation_id: String,
    pub expired_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskReservationReleaseReason {
    Explicit,
    RunTerminal,
    StaleRunReconciled,
    DoctorStaleTaskLock,
    TtlExpired,
    Landed,
}

impl TaskReservationReleaseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::RunTerminal => "run_terminal",
            Self::StaleRunReconciled => "stale_run_reconciled",
            Self::DoctorStaleTaskLock => "doctor_stale_task_lock",
            Self::TtlExpired => "ttl_expired",
            Self::Landed => "landed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskReservationCheckParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    pub requested_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReservationCheckResult {
    pub conflicts: Vec<TaskLockConflict>,
    pub expired_reservations: Vec<ExpiredTaskReservation>,
}

#[derive(Debug, Clone)]
pub struct TaskReservationReserveParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    pub task_ids: Vec<String>,
    /// Original footprint checked against existing holders at the grant boundary.
    pub requested_files: Vec<String>,
    /// Files held by the grant. Empty for work exempt from holding context locks.
    pub stored_files: Vec<String>,
    pub actor: String,
    pub ttl_seconds: u32,
    pub owner_run_id: Option<String>,
    pub owner_metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReservationReserveResult {
    pub reserved: bool,
    pub reservation_id: Option<String>,
    pub expires_at: Option<String>,
    pub reserved_files: Vec<String>,
    pub conflicts: Vec<TaskLockConflict>,
    pub expired_reservations: Vec<ExpiredTaskReservation>,
}

#[derive(Debug, Clone)]
pub struct TaskReservationReleaseParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    pub reservation_id: String,
    pub release_reason: TaskReservationReleaseReason,
    pub release_metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReservationReleaseResult {
    pub released: bool,
    pub released_at: Option<String>,
    pub reservation: Option<ReleasedTaskReservation>,
    pub expired_reservations: Vec<ExpiredTaskReservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveTaskReservation {
    pub reservation_id: String,
    /// A task reservation, or a hard path lease. Advisory leases hold no
    /// paths, so no reader of active holders sees one.
    #[serde(default)]
    pub kind: TaskReservationKind,
    pub workspace_id: Option<String>,
    pub task_ids: Vec<String>,
    pub files: Vec<String>,
    pub actor: String,
    pub created_at: String,
    pub expires_at: String,
    pub owner_run_id: Option<String>,
    pub owner_metadata_json: Option<String>,
}

/// One file reservation as recorded, active or released, for read-only
/// history aggregates. Released rows stay in the table, so this covers every
/// reservation the workspace ever took that has not been migrated away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReservationRecord {
    pub reservation_id: String,
    pub task_ids: Vec<String>,
    pub files: Vec<String>,
    pub created_at: String,
    pub released_at: Option<String>,
    pub owner_run_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleasedTaskReservation {
    pub reservation_id: String,
    pub workspace_id: Option<String>,
    pub task_ids: Vec<String>,
    pub files: Vec<String>,
    pub actor: String,
    pub created_at: String,
    pub expires_at: String,
    pub released_at: String,
    pub owner_run_id: Option<String>,
    pub owner_metadata_json: Option<String>,
    pub release_reason: TaskReservationReleaseReason,
    pub release_metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReservationListResult {
    pub reservations: Vec<ActiveTaskReservation>,
    pub expired_reservations: Vec<ExpiredTaskReservation>,
}

#[derive(Debug, Clone)]
pub struct TaskReservationReleaseByOwnerParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    pub owner_run_id: String,
    pub release_reason: TaskReservationReleaseReason,
    pub release_metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReservationReleaseByOwnerResult {
    pub released_reservations: Vec<ReleasedTaskReservation>,
    pub expired_reservations: Vec<ExpiredTaskReservation>,
}

#[derive(Debug, Clone)]
pub struct TaskReservationOwnedConflictsParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    pub requested_files: Vec<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReservationOwnedConflictsResult {
    pub reservations: Vec<ActiveTaskReservation>,
    pub expired_reservations: Vec<ExpiredTaskReservation>,
}

/// Which coordination dimension a `task_reservations` row belongs to
/// [ADR-0352, ORB-10709].
///
/// File reservations arbitrate between *workers* over paths; a workspace claim
/// arbitrates between *orchestrators* over dispatch authority. They share the
/// table — and therefore the atomic acquisition, TTL, lazy expiry, audit, and
/// release escape hatch already built for reservations — but never each other's
/// rows. Expressing the claim as a whole-workspace file selector instead would
/// block exactly the worker reservations it is meant to leave alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskReservationScope {
    /// A path-scoped worker reservation.
    Files,
    /// A whole-workspace, dispatch-only operator claim.
    WorkspaceClaim,
}

impl TaskReservationScope {
    /// The persisted discriminator. A closed set of `'static` literals, which is
    /// what makes it safe to inline into SQL rather than bind as a parameter.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::WorkspaceClaim => "workspace_claim",
        }
    }
}

/// What holds a `files`-scope `task_reservations` row: a task reservation, or
/// a path lease held by a principal (`docs/design/path-leases`). A lease
/// never becomes a task and a task never holds a lease.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskReservationKind {
    /// Taken by `orbit.task.locks.reserve` or an admission grant. Every row
    /// written before leases existed reads as one.
    #[default]
    TaskReservation,
    /// Held by a principal rather than a task.
    Lease,
}

impl TaskReservationKind {
    /// The persisted discriminator, a closed set of `'static` literals safe
    /// to inline into SQL.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TaskReservation => "task_reservation",
            Self::Lease => "lease",
        }
    }

    pub(crate) fn parse(raw: &str) -> Self {
        match raw {
            "lease" => Self::Lease,
            _ => Self::TaskReservation,
        }
    }
}
