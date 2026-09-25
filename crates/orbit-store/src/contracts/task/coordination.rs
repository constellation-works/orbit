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

pub use orbit_types::task::ExecutionLocation;

/// Authority supplied by the embedding runtime, never deserialized from tool input.
/// The caller must already have session agent capability. SSH login establishes owner
/// access; remote machine labels are attribution, not destination credentials.
#[derive(Debug, Clone)]
pub struct AdmissionIdentity {
    location: ExecutionLocation,
    remote: bool,
}

impl AdmissionIdentity {
    pub fn trusted_local(location: ExecutionLocation) -> Self {
        Self {
            location,
            remote: false,
        }
    }

    pub fn trusted_remote(location: ExecutionLocation) -> Self {
        Self {
            location,
            remote: true,
        }
    }

    pub fn location(&self) -> &ExecutionLocation {
        &self.location
    }
    pub fn is_remote(&self) -> bool {
        self.remote
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionRunContext {
    pub run_id: String,
    pub job_name: String,
    pub machine_name: Option<String>,
}

/// Owner-resolved configuration, included in immutable retry comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionShipContract {
    pub mode: String,
    pub base_branch: String,
    pub landing_branch: String,
    pub review_policy: String,
    pub completion: String,
    pub authorization_reference: Option<String>,
}

/// Wire-protocol version of pull, probe, and lifecycle request/response shapes.
///
/// It versions the distributed-drain protocol alone, not the scoreboard's
/// `ORCHESTRATION_SCHEMA_VERSION` and not MCP's own initialize metadata.
/// Incompatible request/response changes increment it.
pub const DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA: u32 = 1;

/// Receipt-lookup schema, versioned independently of admission so a client
/// upgraded to the owner's binary can reconcile an old request without
/// rewriting the input that request was made with.
pub const ADMISSION_RECEIPT_LOOKUP_SCHEMA: u32 = 1;

/// Ordered pre-admission refusal classes, in the order a caller sees them.
///
/// Selector resolution and session capability are decided by the calling
/// surface before this ladder, because only that surface knows which workspace
/// was addressed and which capabilities the session holds. Everything below is
/// store-owned and shared by admission and the read-only preflight, so a probe
/// cannot report a verdict admission would not reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionRefusal {
    InvalidInput,
    VersionMismatch,
    ShipModeUnsupported,
    ReviewPolicyUnsupported,
}

impl AdmissionRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::VersionMismatch => "version_mismatch",
            Self::ShipModeUnsupported => "ship_mode_unsupported",
            Self::ReviewPolicyUnsupported => "review_policy_unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionRequest {
    pub request_id: String,
    pub caller_version: String,
    pub caller_schema: u32,
    pub caller_review_policy: String,
    pub run_context: AdmissionRunContext,
    pub ship: AdmissionShipContract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionClaimPhase {
    Claimed,
    Running,
    HandedOff,
    Failed,
    Revoked,
    /// The owner landing consumer verified the merge and completed the task.
    Landed,
}

impl ExecutionClaimPhase {
    pub fn protects_footprint(self) -> bool {
        matches!(self, Self::Claimed | Self::Running | Self::HandedOff)
    }
    pub fn is_unsettled(self) -> bool {
        matches!(self, Self::Claimed | Self::Running | Self::HandedOff)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionClaim {
    pub claim_id: String,
    pub task_id: String,
    pub request_id: String,
    pub executed_on: ExecutionLocation,
    pub run_context: AdmissionRunContext,
    pub footprint: Vec<String>,
    pub reservation_id: String,
    pub reservation_expires_at: String,
    pub phase: ExecutionClaimPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionDiagnostic {
    pub task_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionTaskSummary {
    pub id: String,
    pub title: String,
    pub complexity: Option<orbit_types::task::TaskComplexity>,
    pub crew: Option<String>,
    pub context_files: Vec<String>,
}

/// Original response, immutable even when the current claim moves on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionReceipt {
    pub schema_version: u32,
    pub request: AdmissionRequest,
    pub machine_id: String,
    pub claim: Option<ExecutionClaim>,
    pub task: Option<AdmissionTaskSummary>,
    pub invalid_candidates: Vec<AdmissionDiagnostic>,
    pub deferred_conflicts: Vec<AdmissionDiagnostic>,
    pub queue_depth: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionLookup {
    Found {
        receipt: Box<AdmissionReceipt>,
        current_claim: Option<Box<ExecutionClaim>>,
    },
    Expired,
    NotFound,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdmissionStorageUsage {
    pub receipts: u64,
    pub tombstones: u64,
    /// Logical UTF-8 payload bytes; excludes SQLite page/index overhead.
    pub receipt_bytes: u64,
    pub tombstone_bytes: u64,
}

/// Immutable leaf identity. Host labels are diagnostic; machine and run fence ownership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRun {
    pub machine_id: String,
    pub run_id: String,
}

/// Invocation authority supplied by trusted runtime composition, never tool JSON or env.
/// SSH establishes owner access; these fields fence attempts, not destination caller ACLs.
/// The adapter must derive this value from its managed invocation, including when the
/// tool payload omits task/claim context. Public distributed tools remain disabled until
/// that propagation is implemented and verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimInvocation {
    pub(crate) task_id: String,
    pub(crate) claim_id: String,
    pub(crate) machine_id: String,
    pub(crate) run: Option<ClaimRun>,
    pub(crate) operator: bool,
    pub(crate) handoff_observation: Option<HandoffObservation>,
}

impl ClaimInvocation {
    /// Only trusted runtime code may call this constructor; payload labels confer no rights.
    pub fn trusted_worker(
        task_id: String,
        claim_id: String,
        machine_id: String,
        run: Option<ClaimRun>,
    ) -> Self {
        Self {
            task_id,
            claim_id,
            machine_id,
            run,
            operator: false,
            handoff_observation: None,
        }
    }

    /// The embedding runtime must first enforce operator/supervised recovery capability.
    pub fn trusted_operator(task_id: String, claim_id: String, actor: String) -> Self {
        Self {
            task_id,
            claim_id,
            machine_id: actor,
            run: None,
            operator: true,
            handoff_observation: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimEvidence {
    pub summary: Option<String>,
    pub comment: Option<String>,
    pub artifacts: Vec<orbit_types::task::TaskArtifact>,
}

/// Worker-owned documents and coordination metadata. Lifecycle transitions
/// remain governed by the claim state machine, including typed review handoff.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimWorkerUpdate {
    pub evidence: ClaimEvidence,
    pub plan: Option<String>,
    pub context_files: Option<Vec<String>>,
    pub external_refs: Vec<orbit_types::task::ExternalRef>,
    pub status: Option<TaskStatus>,
    pub expected_status: Option<TaskStatus>,
    pub status_note: Option<String>,
}

/// Internal lifecycle operations. Typed handoffs validate owner observations and durable
/// evidence inside the ownership/phase boundary. Approval records completion authority;
/// no operation here executes an external merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaimMutation {
    Bind {
        run: ClaimRun,
        ship: AdmissionShipContract,
    },
    Evidence(ClaimEvidence),
    Update(ClaimWorkerUpdate),
    /// Friction allocation and publication share the claim's commit transaction.
    Friction(super::super::FrictionAddParams),
    /// Legacy serialized shape retained for reading only; new writes are refused.
    Handoff(ClaimEvidence),
    AcceptHandoff(orbit_types::workflow::handoff::TaskHandoff),
    ApproveHandoff {
        handoff_id: String,
        candidate: orbit_types::workflow::handoff::HandoffCandidate,
    },
    RevokeHandoff {
        handoff_id: String,
        reason: String,
    },
    Fail(ClaimEvidence),
    Recover {
        status: TaskStatus,
        reason: String,
    },
    /// Durable guard for the later external merge consumer. Only operators can record
    /// or reconcile intent; revocation cannot race past an unresolved intent.
    MergeIntent {
        intent_id: String,
        resolved: bool,
        evidence: String,
    },
    /// Reserve the single live landing attempt for a handoff, or record the
    /// owner job that carries the reserved attempt. Handoff identity is the
    /// deduplication key; a stopped attempt reopens as the next attempt.
    DispatchLanding {
        handoff_id: String,
        job_run_id: Option<String>,
    },
    /// Complete an authorized landing against verified external merge evidence.
    /// Refused while a merge intent is unresolved or the authority is stale.
    CompleteLanding {
        handoff_id: String,
        evidence: String,
    },
    /// Stop the live attempt with durable evidence, leaving the task in review.
    StopLanding {
        handoff_id: String,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimInspection {
    pub claim: ExecutionClaim,
    pub bound_run: Option<ClaimRun>,
    pub created_at: String,
    pub updated_at: String,
    pub last_event: String,
    pub age_seconds: Option<i64>,
    pub unresolved_merge_intent: Option<String>,
    pub landing_invalidated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimMutationResult {
    pub claim_id: String,
    pub phase: ExecutionClaimPhase,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub friction: Option<orbit_types::record::FrictionRecord>,
}

/// SQL effects checked and published at the journal's existing commit point.
#[derive(Debug, Clone, Default)]
pub(crate) struct ClaimCommitEffects {
    pub replacements: Vec<(TaskCoordinationRow, TaskCoordinationRow)>,
    pub release_reservation: Option<String>,
    pub friction: Option<(super::super::FrictionAddParams, String)>,
    pub execution_origin: Option<ExecutionLocation>,
    pub worker_update: Option<ClaimWorkerUpdate>,
}

/// Owner observations from Git/provider identity and repository validation policy.
/// Not deserializable: adapters must obtain these independently of handoff JSON.
/// For already-landed delivery, the adapter must run the existing typed evidence,
/// scope, ancestry, delivery-marker and clean-tree checks before constructing this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffObservation {
    pub candidate: orbit_types::workflow::handoff::HandoffCandidate,
    pub required_commands: Vec<String>,
}

impl ClaimInvocation {
    /// Trusted owner-domain seam; never fill observations from worker payloads.
    pub fn with_handoff_observation(mut self, observation: HandoffObservation) -> Self {
        self.handoff_observation = Some(observation);
        self
    }
}
