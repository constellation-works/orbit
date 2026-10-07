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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdmissionRunContext {
    pub run_id: String,
    pub job_name: String,
    pub machine_name: Option<String>,
}

/// Owner-resolved configuration, included in immutable retry comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdmissionShipContract {
    pub mode: String,
    pub base_branch: String,
    pub landing_branch: String,
    /// The owner's `review.before_pr` when it resolved this contract
    /// [ORB-13992]. Contracts recorded before revision 5 carried a
    /// `review_policy` admitted only as `none`, which reads as `false`.
    #[serde(default)]
    pub before_pr: bool,
    pub completion: String,
    pub authorization_reference: Option<String>,
    /// The owner's captured before-PR review contract: present exactly when
    /// `before_pr` is on [ORB-13895]. A handoff's review evidence is judged
    /// against it, never against the owner's settings at handoff time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<AdmissionReviewContract>,
}

/// The review a claimed leaf must run before it opens a pull request, as the
/// owner resolved it when the claim was admitted [ORB-13895]. Its timing is
/// the ship contract's `before_pr`; the rest is what the leaf's gate and the
/// owner's acceptance hold it to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdmissionReviewContract {
    /// The review evidence contract version the owner reads
    /// (`REVIEW_CONTRACT_VERSION`). A certificate under another version is
    /// refused rather than reinterpreted.
    pub contract_version: u32,
    /// The owner's `operation.review_crew`, when one is set. A handoff's
    /// reviewer must be this crew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// The owner's `review.minutes` budget for the leaf's review.
    pub budget: orbit_types::workflow::ReviewBudget,
    /// Owner-required candidate checks captured at claim admission. `None`
    /// means this is a legacy ship contract without enough evidence to
    /// establish review validation; an empty list is an explicit no-check
    /// contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_validation_commands: Option<Vec<String>>,
}

impl AdmissionShipContract {
    /// Whether `review` matches `before_pr` and is itself well formed.
    #[must_use]
    pub fn review_contract_consistent(&self) -> bool {
        match &self.review {
            None => !self.before_pr,
            Some(review) => {
                self.before_pr
                    && review.contract_version == orbit_types::workflow::REVIEW_CONTRACT_VERSION
                    && review
                        .crew
                        .as_deref()
                        .is_none_or(|crew| !crew.trim().is_empty())
                    && review.required_validation_commands.is_some()
            }
        }
    }
}

/// Wire-protocol version of pull, probe, and lifecycle request/response shapes.
///
/// It versions the distributed-drain protocol alone, not the scoreboard's
/// `ORCHESTRATION_SCHEMA_VERSION` and not MCP's own initialize metadata.
/// Retained for persisted requests and lifecycle semantics. Pull request field
/// compatibility is now derived by [`distributed_drain_protocol_fingerprint`],
/// so an additive field needs no manual bump to detect skew. Revision 2 adds executor crew capabilities;
/// revision 3 adds handoff footprint widening; revision 4 adds the executor's
/// host OS; revision 5 replaces both endpoints' review-policy labels with
/// their captured `review.before_pr` switch [ORB-13992]; revision 6 adds the
/// ship contract's captured `review`, the executor's `review_gate` and the
/// handoff's before-PR review evidence [ORB-13895]; revision 7 sends
/// `review_gate` on `orbit.task.pull`, which revision 6 owners reject
/// [ORB-13908]; revision 8 captures the owner's required validation
/// commands in the before-PR review contract [ORB-14192]; revision 9 adds
/// NoDiff claim delivery and owner-verified clean-base completion [ORB-14259];
/// revision 10 adds the settlement's typed failure, the `leaf_released` crew
/// exclusion source and the receipt's resumable candidate [ORB-14257].
pub const DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA: u32 = 10;

/// The pull wire shape, derived from the same request and nested types that
/// admission deserializes. No field list or manually bumped revision can drift
/// from these types. Keep the draft explicit across generator upgrades.
pub fn admission_request_schema() -> &'static serde_json::Value {
    static SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        schemars::generate::SchemaSettings::draft07()
            // Prose edits must not create wire skew. Transform schema nodes,
            // preserving request properties actually named title/description.
            .with_transform(schemars::transform::RecursiveTransform(
                |schema: &mut schemars::Schema| {
                    schema.remove("title");
                    schema.remove("description");
                },
            ))
            .into_generator()
            .into_root_schema_for::<AdmissionRequest>()
            .to_value()
    })
}

/// SHA-256 of the generated pull request schema, including nested contracts.
/// Both endpoints compute this from their running build, independently of the
/// release version and the legacy protocol revision.
pub fn distributed_drain_protocol_fingerprint() -> &'static str {
    static FINGERPRINT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FINGERPRINT.get_or_init(|| {
        orbit_common::security::release::sha256_hex(
            admission_request_schema().to_string().as_bytes(),
        )
    })
}

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
    ProtocolSkew,
    ProtocolMismatch,
    VersionMismatch,
    ShipModeUnsupported,
    /// The owner captured `review.before_pr = true` and the executor's leaf
    /// does not declare that it runs the before-PR gate, or the ship mode is
    /// not the PR route the gate runs on [ORB-13908]. The executor's own
    /// switch never refuses. After-landing review never refuses: it runs on
    /// the owner after landing.
    #[serde(alias = "review_policy_unsupported")]
    BeforePrUnsupported,
}

impl AdmissionRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::ProtocolSkew => "protocol_skew",
            Self::VersionMismatch => "version_mismatch",
            Self::ProtocolMismatch => "protocol_mismatch",
            Self::ShipModeUnsupported => "ship_mode_unsupported",
            Self::BeforePrUnsupported => "before_pr_unsupported",
        }
    }
}

/// The crews a pulling executor can run, declared on each request [ORB-13941].
///
/// The owner admits a candidate only when the crew it would run as on this
/// executor — its own `task.crew`, or `default_crew` for a task naming none —
/// is runnable here. A task the executor cannot run is skipped, not refused:
/// it stays in the backlog for the owner or another follower.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdmissionCrewCapability {
    /// Crews the executor's window preflight found runnable, by registry
    /// name. `None` when the window took no preflight: then any crew not in
    /// `excluded` is admissible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runnable: Option<Vec<String>>,
    /// The crew a task naming none runs as on the executor. `None` admits no
    /// crew-less task, since its leaf would have no crew to run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_crew: Option<String>,
    /// Crews never admitted to this executor for the rest of its window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<orbit_types::workflow::CrewExclusion>,
}

impl AdmissionCrewCapability {
    /// Why a task whose own crew is `task_crew` cannot run on this executor,
    /// or `None` when it can.
    #[must_use]
    pub fn unrunnable_reason(&self, task_crew: Option<&str>) -> Option<String> {
        let named = task_crew.map(str::trim).filter(|crew| !crew.is_empty());
        let Some(crew) = named.or(self.default_crew.as_deref()) else {
            return Some(
                "names no crew and the executor has no default crew to run it as".to_string(),
            );
        };
        let via = if named.is_some() {
            String::new()
        } else {
            " (the executor's default crew)".to_string()
        };
        if let Some(exclusion) = self
            .excluded
            .iter()
            .find(|exclusion| exclusion.crew == crew)
        {
            return Some(format!(
                "crew `{crew}`{via} is excluded on the executor: {}",
                exclusion.reason
            ));
        }
        match &self.runnable {
            Some(runnable) if !runnable.iter().any(|name| name == crew) => Some(format!(
                "crew `{crew}`{via} is not runnable on the executor"
            )),
            _ => None,
        }
    }

    fn malformed(&self) -> bool {
        let blank = |name: &str| name.trim().is_empty();
        self.runnable.iter().flatten().any(|name| blank(name))
            || self.default_crew.as_deref().is_some_and(blank)
            || self.excluded.iter().any(|exclusion| blank(&exclusion.crew))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdmissionRequest {
    pub request_id: String,
    pub caller_version: String,
    pub caller_schema: u32,
    /// Type-derived request fingerprint. Optional when reading persisted
    /// requests from before fingerprint negotiation; new followers declare it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_fingerprint: Option<String>,
    /// The executor's captured `review.before_pr` [ORB-13992]. Diagnostic
    /// only: a claimed leaf runs the review the ship contract captured, never
    /// the executor's own [ORB-13908]. Requests recorded before revision 5
    /// carried a `caller_review_policy`, which reads as `false`.
    #[serde(default)]
    pub caller_before_pr: bool,
    /// Whether the executor's claimed leaf runs the before-PR gate the ship
    /// contract's `review` captures [ORB-13895]. An owner with
    /// `review.before_pr` on admits only an executor that declares it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub review_gate: bool,
    pub run_context: AdmissionRunContext,
    pub ship: AdmissionShipContract,
    /// What the executor can run. Absent for an owner-local admission and
    /// for callers that place no crew restriction: then every crew is
    /// admissible, as before [ORB-13941].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crews: Option<AdmissionCrewCapability>,
    /// The executor's host OS, matched against each candidate's `os:` tags.
    /// Absent from an executor on an OS outside the `os:` namespace: only
    /// tasks without an `os:` tag are admitted to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<orbit_types::task::HostOs>,
}

impl AdmissionRequest {
    /// Whether the declared crew capability is malformed (a blank name).
    #[must_use]
    pub fn crews_malformed(&self) -> bool {
        self.crews
            .as_ref()
            .is_some_and(AdmissionCrewCapability::malformed)
    }
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
    /// [ORB-14261] The owner's landing stopped because the handed-off
    /// candidate conflicts with, or is stale against, its base. The handoff's
    /// authority is revoked and the task waits `in-progress` for one repair
    /// leaf; admitting that leaf settles this claim as `revoked`.
    RepairPending,
}

impl ExecutionClaimPhase {
    pub fn protects_footprint(self) -> bool {
        matches!(
            self,
            Self::Claimed | Self::Running | Self::HandedOff | Self::RepairPending
        )
    }
    pub fn is_unsettled(self) -> bool {
        matches!(
            self,
            Self::Claimed | Self::Running | Self::HandedOff | Self::RepairPending
        )
    }
}

/// [ORB-14261] What a repair claim carries: the earlier claim whose landing
/// stopped on a conflicting or stale base, the handoff it delivered, the
/// candidate that handoff preserved and the stop's evidence. A claim carries
/// at most one, and a claim that carries one is the task's only automatic
/// repair: its own repairable stop blocks the task instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRepair {
    /// The claim this repair supersedes.
    pub repairs_claim_id: String,
    /// The handoff whose landing stopped.
    pub handoff_id: String,
    /// The candidate that handoff delivered, as the owner accepted it.
    pub candidate: orbit_types::workflow::handoff::HandoffCandidate,
    /// The landing stop's evidence.
    pub stop_evidence: String,
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
    /// Present on a claim admitted to repair an earlier claim's stopped
    /// landing [ORB-14261]. The leaf restores this candidate instead of
    /// implementing the task from scratch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<ClaimRepair>,
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
    /// [ORB-14257] The candidate an earlier claim of the task preserved,
    /// for this claim's leaf to resume instead of implementing afresh. The
    /// owner offers it only while no operator discarded it and the task's
    /// spec is unchanged since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_candidate: Option<ClaimCandidateRef>,
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
    /// Ready candidates skipped because the executor cannot run their crew.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crew_unavailable: Vec<AdmissionDiagnostic>,
    /// Ready candidates skipped because their `os:` tags name no OS the
    /// executor runs. They stay in the backlog for a host that does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub os_unavailable: Vec<AdmissionDiagnostic>,
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
/// tool payload omits task/claim context. The owner's registered bind and settle tools
/// build it from the trusted session machine, never from a machine named in their input.
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
    /// Set on a release whose leaf could not use its provider [ORB-13941].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_unavailable: Option<ProviderUnavailable>,
    /// [ORB-14258] Set on a release whose leaf's required command fails on
    /// its base exactly as on the candidate. The owner records the hold with
    /// the release, so its admission withholds the task until the held command
    /// passes on a new base tip.
    /// An owner that predates the field releases the task unheld.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_red: Option<orbit_types::workflow::BaselineRedHold>,
    /// [ORB-13907] On a failure settlement only: the leaf's final-recovery
    /// decision, which the owner applies to its task once the claim has
    /// failed. An owner that predates the field ignores it and only blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_recovery: Option<ClaimFinalRecovery>,
    /// [ORB-14257] On a launched leaf's failure or release: why it ended
    /// without its handoff. The owner blocks the task only for a class that
    /// [blocks](orbit_types::workflow::ClaimFailureClass::blocks), and
    /// releases any other within its per-task release budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ClaimFailure>,
    /// Set on a release whose leaf's before-PR review settled into an
    /// evidence hold. The owner keeps the task in progress with
    /// `review_awaiting_evidence` as its latest decision, so receipt of the
    /// named evidence queues a fresh review. An owner that predates the field
    /// releases the task unheld.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_hold: Option<orbit_types::workflow::ReviewEvidenceHold>,
}

/// Why a launched claimed leaf ended without its typed handoff [ORB-14257].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimFailure {
    pub class: orbit_types::workflow::ClaimFailureClass,
    /// What ended the leaf — the cancel, or its failed step's error — bounded.
    pub reason: String,
    /// The crew the leaf ran as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// The candidate the leaf committed before it ended, so a later claim
    /// can start from it rather than from the base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<ClaimCandidateRef>,
}

/// The committed candidate a claimed leaf ended with: the branch it pushed,
/// or, before its push, the local branch it prepared — so a base conflict at
/// synchronization keeps it too — or, for a claimed-local leaf, its committed
/// worktree branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimCandidateRef {
    pub branch: String,
    pub head_sha: String,
    /// The pull request the leaf opened for it, when it got that far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<String>,
    /// The leaf run that produced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<String>,
    /// The first step of the leaf that did not complete; `candidate_resume`
    /// picks its repair trigger from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_step_id: Option<String>,
    /// Whether the branch reached `origin`. A candidate that did not, and
    /// has no [`Self::durable_ref`], resumes only on the host that made it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub published: bool,
    /// [ORB-14338] The ref on `origin` the leaf pushed its unpublished
    /// candidate to before it ended (`refs/orbit/candidates/<task>/<run>`),
    /// so a claim on any host can fetch it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_ref: Option<String>,
    /// [ORB-14338] Why the leaf could not push its unpublished candidate to a
    /// durable ref; the candidate stays on the host that made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carry_failure: Option<String>,
}

impl ClaimCandidateRef {
    /// Whether a host other than the one that made it can fetch the
    /// candidate from `origin`: its branch was pushed, or the leaf carried it
    /// to a durable ref [ORB-14338].
    #[must_use]
    pub fn durable(&self) -> bool {
        self.published || self.durable_ref.is_some()
    }
}

/// Why the owner handed a claim no kept candidate, so its leaf implements the
/// task afresh [ORB-14338]. The owner records it in the task's history as a
/// `candidate_resume` event rather than falling back silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateFreshReason {
    /// The candidate was never pushed and could not be made durable, and
    /// the claim runs on a host other than the one that made it.
    NotDurable,
    /// The task's description, acceptance criteria or selectors changed
    /// since the candidate was kept.
    SpecChanged,
    /// An operator discarded the candidate since it was kept.
    Discarded,
}

impl CandidateFreshReason {
    /// The reason's wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotDurable => "not_durable",
            Self::SpecChanged => "spec_changed",
            Self::Discarded => "discarded",
        }
    }
}

/// A candidate the owner kept from a claim's failure or release, with the
/// task spec it answered to [ORB-14257].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreservedClaimCandidate {
    pub candidate: ClaimCandidateRef,
    /// The task's spec digest when the claim settled; a later change to the
    /// description, criteria or selectors retires the candidate.
    pub task_spec_digest: String,
    pub recorded_at: String,
}

/// A typed release the owner applied, kept on the claim's lifecycle state so
/// the task's release budget can count it [ORB-14257].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimReleaseRecord {
    pub class: orbit_types::workflow::ClaimFailureClass,
    pub reason: String,
    pub released_at: String,
    /// Set on the settlement that exhausted the budget and blocked the task;
    /// releases before it no longer count.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub budget_exhausted: bool,
}

/// A claimed leaf ended because its crew's provider could not be used on the
/// executing host (an authentication failure, for instance), not because the
/// work failed. Its claim is released to the backlog and the crew is excluded
/// for the rest of the executor's drain window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderUnavailable {
    /// The crew the leaf ran as.
    pub crew: Option<String>,
    /// The provider's own diagnostic, bounded.
    pub reason: String,
}

/// [ORB-13907] A claimed leaf's final-recovery decision, carried to the owner
/// by the leaf's failure settlement instead of being written by the follower.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimFinalRecovery {
    /// The leaf run whose final recovery decided.
    pub run_id: String,
    /// The decision as the leaf's agent proposed it. The owner verifies a
    /// `complete_no_diff` commit against its own base branch.
    pub decision: orbit_types::workflow::FinalRecoveryDecision,
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
    /// The executor gives an unfinished claim back: the task returns to the
    /// backlog and the claim is revoked. The summary is the reason and is
    /// required; the comment, when present, is posted on the task.
    Release(ClaimEvidence),
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
    /// A `repairable` stop — the candidate conflicts with, or is stale
    /// against, its base — instead moves an original claim to
    /// `repair_pending`, and blocks the task when the claim is already its
    /// repair [ORB-14261].
    StopLanding {
        handoff_id: String,
        reason: String,
        #[serde(default)]
        repairable: bool,
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
    /// [ORB-14257] The typed failure release the owner applied to this claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<ClaimReleaseRecord>,
    /// [ORB-14257] The candidate this claim's failure or release preserved,
    /// offered to the task's next claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserved_candidate: Option<PreservedClaimCandidate>,
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
/// For NoDiff it must re-run the clean-tree checkpoint verifier against the
/// owner's current base and pinned report, without trusting an executor branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffObservation {
    /// Owner-observed eligible additions outside the original footprint.
    pub footprint_widening: Vec<String>,
    pub candidate: orbit_types::workflow::handoff::HandoffCandidate,
    pub required_commands: Vec<String>,
    /// The completion authority the owner's configuration grants claimed
    /// handoffs at the moment of this decision, read by trusted owner code
    /// from its own settings. `None` means every handoff waits for an
    /// operator's approval.
    pub owner_completion_authority: Option<String>,
    /// What the owner observed about a before-PR handoff's review evidence.
    /// Required to accept one; `None` for a handoff carrying none.
    pub review: Option<HandoffReviewObservation>,
}

/// The owner's own reading of the facts a before-PR certificate stands on
/// that the claim journal cannot check itself [ORB-13895].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffReviewObservation {
    /// The reviewed base the owner checked; it must be the handoff's.
    pub reviewed_base_sha: String,
    /// Whether that base is the owner-observed candidate base or one of its
    /// ancestors, in the owner's checkout.
    pub reviewed_base_is_ancestor: bool,
    /// The owner's repository identity, which after-landing coverage matches
    /// certificates against.
    pub repository: String,
}

/// Why an owner refused a handoff's review disposition [ORB-13895]. The code
/// leads the refusal message, so a caller reading only the error can tell
/// which check failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffReviewRefusal {
    /// The claim captured `review.before_pr` and the handoff carries no
    /// before-PR evidence.
    ReviewEvidenceMissing,
    /// The claim captured no before-PR review and the handoff claims one.
    ReviewEvidenceUnexpected,
    /// The reviewer's verdict does not let the candidate open a PR.
    ReviewNotPassed,
    /// The reviewed head is not the handed-off candidate.
    ReviewedHeadMismatch,
    /// The reviewed base is not the owner's candidate base or an ancestor of it.
    ReviewedBaseNotAncestor,
    /// The certificate artifact is missing, changed, unreadable, or disagrees
    /// with the evidence, the candidate, or the owner's repository.
    ReviewCertificateMismatch,
    /// The reviewer or the certificate's contract is not the one the claim
    /// captured.
    ReviewContractMismatch,
}

impl HandoffReviewRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReviewEvidenceMissing => "review_evidence_missing",
            Self::ReviewEvidenceUnexpected => "review_evidence_unexpected",
            Self::ReviewNotPassed => "review_not_passed",
            Self::ReviewedHeadMismatch => "reviewed_head_mismatch",
            Self::ReviewedBaseNotAncestor => "reviewed_base_not_ancestor",
            Self::ReviewCertificateMismatch => "review_certificate_mismatch",
            Self::ReviewContractMismatch => "review_contract_mismatch",
        }
    }
}

impl ClaimInvocation {
    /// Trusted owner-domain seam; never fill observations from worker payloads.
    pub fn with_handoff_observation(mut self, observation: HandoffObservation) -> Self {
        self.handoff_observation = Some(observation);
        self
    }
}
