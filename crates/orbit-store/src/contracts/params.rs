use chrono::{DateTime, Utc};
use orbit_types::identity::OrbitId;
use orbit_types::task::{
    ExternalRef, TaskArtifact, TaskComment, TaskComplexity, TaskHistoryEntry, TaskPriority,
    TaskRelation, TaskStatus, TaskType,
};
use orbit_types::workflow::JobRunState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceConfig {
    pub schema_version: u32,
    pub workspace_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCreateParams {
    pub actor: String,
    pub parent_id: Option<OrbitId>,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub dependencies: Vec<OrbitId>,
    pub relations: Vec<TaskRelation>,
    pub tags: Vec<String>,
    pub required_tools: Vec<String>,
    pub plan: String,
    pub execution_summary: String,
    pub context_files: Vec<String>,
    /// Deprecated compatibility metadata. Task bundles do not persist a
    /// task-specific working directory; task context selectors are stored
    /// relative to the repository root by the application layer.
    pub workspace_path: Option<String>,
    /// Deprecated compatibility metadata retained for callers that still
    /// construct the v2 create contract.
    pub repo_root: Option<String>,
    pub created_by: Option<String>,
    pub planned_by: Option<String>,
    pub implemented_by: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub complexity: Option<TaskComplexity>,
    pub task_type: TaskType,
    pub external_refs: Vec<ExternalRef>,
    pub source_task_id: Option<String>,
    pub crew: Option<String>,
    pub orchestrator: Option<String>,
    pub comments: Vec<TaskComment>,
}

/// Parameters for a partial update to an existing task.
///
/// Fields that are `None` are left unchanged. Fields of type `Option<Option<T>>`
/// follow the "outer = whether to update, inner = new value" convention:
/// - `None`           → leave the field untouched
/// - `Some(Some(v))`  → set the field to `v`
/// - `Some(None)`     → explicitly clear the field (set it to null/absent)
#[derive(Debug, Default, Clone)]
pub struct TaskDocumentUpdateParams {
    pub actor: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub acceptance_criteria: Option<Vec<String>>,
    pub dependencies: Option<Vec<OrbitId>>,
    pub relations: Option<Vec<TaskRelation>>,
    pub tags: Option<Vec<String>>,
    pub plan: Option<String>,
    pub execution_summary: Option<String>,
    pub context_files: Option<Vec<String>>,
    pub created_by: Option<Option<String>>,
    pub planned_by: Option<Option<String>>,
    pub implemented_by: Option<Option<String>>,
    pub priority: Option<TaskPriority>,
    pub complexity: Option<TaskComplexity>,
    pub task_type: Option<TaskType>,
    pub external_refs: Option<Vec<ExternalRef>>,
    pub pr_status: Option<Option<String>>,
    pub source_task_id: Option<Option<String>>,
    pub job_run_id: Option<Option<String>>,
    pub crew: Option<Option<String>>,
    pub orchestrator: Option<Option<String>>,
}

#[derive(Debug, Default, Clone)]
pub struct TaskHistoryUpdateParams {
    pub actor: String,
    pub status: Option<TaskStatus>,
    pub status_event: Option<String>,
    pub status_note: Option<String>,
    pub append_history: Vec<TaskHistoryEntry>,
    pub append_comments: Vec<TaskComment>,
    /// [ORB-11305] Compare-and-set guard: when `Some`, the write is applied
    /// only if the task's *persisted* status at write time is one of these.
    ///
    /// The check runs inside the per-task exclusive file lock, after the
    /// bundle is re-read, so a status a caller observed before calling cannot
    /// go stale in the gap. `None` keeps the unconditional last-writer-wins
    /// behavior every other caller relies on.
    pub expected_status: Option<Vec<TaskStatus>>,
}

/// One task-bundle mutation whose freshness guard, durable receipt, history
/// event, and envelope changes share the envelope publish commit point.
#[derive(Debug, Clone)]
pub struct AtomicTaskMutationParams {
    pub actor: String,
    pub operation_id: String,
    pub expected_context_files: Vec<String>,
    pub expected_status: TaskStatus,
    pub expected_complexity: Option<TaskComplexity>,
    pub context_files: Vec<String>,
    pub status: TaskStatus,
    pub complexity: TaskComplexity,
    pub event_type: String,
    pub event_note: String,
    /// Human-inspectable evidence for the decision committed by this mutation.
    pub audit_note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicTaskMutationOutcome {
    Applied,
    AlreadyApplied,
    Stale,
}

#[derive(Debug, Default, Clone)]
pub struct TaskArtifactUpdateParams {
    pub actor: String,
    /// Trusted executor context supplied by Core, never parsed from tool input.
    pub owner_run_id: Option<String>,
    /// Artifact files to write under the task bundle `artifacts/` directory.
    /// Existing files at the same relative path are overwritten.
    pub upsert_artifacts: Vec<TaskArtifact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLockHolder {
    Task,
    Reservation,
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
}

impl TaskReservationReleaseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::RunTerminal => "run_terminal",
            Self::StaleRunReconciled => "stale_run_reconciled",
            Self::DoctorStaleTaskLock => "doctor_stale_task_lock",
            Self::TtlExpired => "ttl_expired",
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
    pub requested_files: Vec<String>,
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
    pub workspace_id: Option<String>,
    pub task_ids: Vec<String>,
    pub files: Vec<String>,
    pub actor: String,
    pub created_at: String,
    pub expires_at: String,
    pub owner_run_id: Option<String>,
    pub owner_metadata_json: Option<String>,
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

/// The current holder of a workspace claim, as reported to a contender.
///
/// `machine_id` and `session_id` are diagnostics only. The claim is keyed on
/// [`WorkspaceClaimAcquireResult::claim_token`], never on session identity: MCP
/// session identity is minted per connection and cleared when client-supplied,
/// so a reconnecting client would orphan the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceClaimHolder {
    pub claim_id: String,
    pub workspace_id: Option<String>,
    pub actor: String,
    pub created_at: String,
    pub expires_at: String,
    pub machine_id: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceClaimAcquireParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    pub actor: String,
    pub ttl_seconds: u32,
    pub machine_id: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceClaimAcquireResult {
    pub acquired: bool,
    /// The minted bearer token, returned exactly once to the acquiring holder.
    /// Never populated on the contention path, so a refused contender cannot
    /// learn the incumbent's token from its own refusal.
    pub claim_token: Option<String>,
    pub claim: Option<WorkspaceClaimHolder>,
    /// The incumbent when acquisition was refused.
    pub conflict: Option<WorkspaceClaimHolder>,
    pub expired_claims: Vec<ExpiredTaskReservation>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceClaimReleaseParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    /// The holder's token. Required unless `force` is set.
    pub claim_token: Option<String>,
    /// Release the claim without its token — the audited escape hatch for a
    /// holder that has gone away.
    pub force: bool,
    pub released_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceClaimReleaseResult {
    pub released: bool,
    pub forced: bool,
    pub released_at: Option<String>,
    /// The claim that was released, or — when a token release was refused — the
    /// incumbent it did not match.
    pub claim: Option<WorkspaceClaimHolder>,
    pub expired_claims: Vec<ExpiredTaskReservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceClaimStatusResult {
    pub claim: Option<WorkspaceClaimHolder>,
    pub expired_claims: Vec<ExpiredTaskReservation>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceClaimCheckParams {
    pub workspace_orbit_dir: String,
    pub workspace_id: Option<String>,
    /// The token the caller presented, if any.
    pub claim_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceClaimCheckResult {
    /// The active claim after lazy expiry, or `None` when the workspace is
    /// unclaimed. An unclaimed workspace gates nothing.
    pub claim: Option<WorkspaceClaimHolder>,
    /// Whether the presented token matches the active claim. Always `false`
    /// when `claim` is `None`: there is no token to match, and the caller must
    /// read the absent claim rather than a truthy match.
    pub token_matches: bool,
    pub expired_claims: Vec<ExpiredTaskReservation>,
}

#[derive(Debug, Clone, Default)]
pub struct JobRunQuery {
    pub job_id: Option<String>,
    pub state: Option<JobRunState>,
    /// Whether to include only states for which `JobRunState::is_terminal()`
    /// returns true. Applied before ordering and limiting.
    pub terminal_only: bool,
    /// Whether to include only `pending` and `running` rows. Applied before
    /// ordering, limiting, and step hydration so admission and orphan scans
    /// never read terminal history [ORB-11762]. Independent of `state` and
    /// `terminal_only`; combining this with `terminal_only` yields no rows.
    pub active_only: bool,
    pub created_since: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
    /// Which timestamp `limit` truncates against. Defaults to `CreatedAt` so
    /// existing CLI/history callers keep their current ordering.
    pub order_by: JobRunOrder,
}

/// Which timestamp a bounded [`JobRunQuery`] orders and truncates by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JobRunOrder {
    /// `created_at DESC, run_id ASC` — the historical list/CLI default.
    #[default]
    CreatedAt,
    /// The same "most recent activity" timestamp displayed runs are ranked
    /// by: `finished_at`, else `started_at`, else `created_at`, each DESC
    /// with `run_id ASC` as the deterministic tiebreak.
    Recency,
}
