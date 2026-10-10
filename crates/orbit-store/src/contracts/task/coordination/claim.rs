//! Execution claim identity, evidence, and preserved candidate contracts.

use serde::{Deserialize, Serialize};

use super::{
    AdmissionRunContext, ClaimFinalRecovery, ExecutionLocation, HandoffObservation,
    ProviderUnavailable,
};

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

impl ClaimInvocation {
    /// Trusted owner-domain seam; never fill observations from worker payloads.
    pub fn with_handoff_observation(mut self, observation: HandoffObservation) -> Self {
        self.handoff_observation = Some(observation);
        self
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
    /// [ORB-14634] Set on a `transient` release whose leaf held its claim
    /// while the forge refused its push, and released it once its retry
    /// window closed. The forge, not the host or the crew, refused it, so the
    /// release excludes neither from the drain's window. An owner that
    /// predates the field keeps the task from that drain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge_hold: Option<orbit_types::workflow::ForgeUnavailableHold>,
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
    /// [ORB-14695] Set on a `provider` failure whose provider reported its
    /// account's usage limit. The limit says nothing about the task, so its
    /// release does not count against the task's release budget. An owner
    /// that predates the field counts it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provider_limit: bool,
}

impl ClaimFailure {
    /// Whether a release for this failure counts against the task's release
    /// budget: its class is [budgeted](orbit_types::workflow::ClaimFailureClass::budgeted)
    /// and it is not a provider usage limit.
    #[must_use]
    pub fn budgeted(&self) -> bool {
        self.class.budgeted() && !self.provider_limit
    }
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

/// Why the owner handed a run no kept candidate, so it implements the task
/// afresh [ORB-14338]. The owner records it in the task's history as a
/// `candidate_resume` event rather than falling back silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateFreshReason {
    /// The candidate was never pushed and could not be made durable, and
    /// the claim runs on a host other than the one that made it.
    NotDurable,
    /// The task's description or acceptance criteria changed since the
    /// candidate was kept. A selector edit does not retire it.
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
    /// description or criteria retires the candidate.
    pub task_spec_digest: String,
    pub recorded_at: String,
}

/// The candidate a task's latest claim settlement kept, as the owner offers
/// it to one machine's run of the task: a claim's leaf through admission
/// [ORB-14338], or the owner's own run after a failed claim [ORB-14603].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptClaimCandidate {
    /// The claim whose settlement kept it.
    pub claim_id: String,
    /// The machine that claim executed on, which committed the candidate.
    pub machine_id: String,
    pub candidate: ClaimCandidateRef,
    /// Why the run implements afresh instead, with the operator-facing
    /// detail; `None` when it resumes the candidate.
    pub fresh: Option<(CandidateFreshReason, String)>,
}
