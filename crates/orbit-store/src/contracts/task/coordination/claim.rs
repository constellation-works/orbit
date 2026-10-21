//! Execution claim identity, evidence, and preserved candidate contracts.

use serde::{Deserialize, Serialize};

use orbit_types::workflow::ReviewLedger;

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
    pub(crate) provider_hold: Option<orbit_types::workflow::ProviderFailureHold>,
    pub(crate) attribution: ClaimAttribution,
}

/// The crew and model a claimed leaf ran as, as its follower reports them.
/// Attribution only: the owner keeps them on the claim's settlement record
/// for the scorecards and never consults them for validation, a settlement's
/// outcome or review-crew selection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimAttribution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew_model: Option<String>,
}

impl ClaimAttribution {
    /// Whether neither field is reported.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.crew.is_none() && self.crew_model.is_none()
    }
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
            provider_hold: None,
            attribution: ClaimAttribution::default(),
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
            provider_hold: None,
            attribution: ClaimAttribution::default(),
        }
    }
}

impl ClaimInvocation {
    /// Trusted owner-domain seam; never fill observations from worker payloads.
    pub fn with_handoff_observation(mut self, observation: HandoffObservation) -> Self {
        self.handoff_observation = Some(observation);
        self
    }

    /// [ORB-15573] The provider failure hold the owner computed for a release
    /// whose leaf its provider refused. The release records it as the task's
    /// latest status decision; never fill it from a worker payload.
    pub fn with_provider_hold(mut self, hold: orbit_types::workflow::ProviderFailureHold) -> Self {
        self.provider_hold = Some(hold);
        self
    }

    /// The crew and model the follower reported beside its settlement, on
    /// the settle call rather than inside it. A typed handoff carries no
    /// [`ClaimEvidence`], so this is how its leaf's attribution reaches the
    /// owner. It is not part of the mutation's identity, so a retry that
    /// reports it differently still replays the recorded outcome.
    pub fn with_attribution(mut self, attribution: ClaimAttribution) -> Self {
        self.attribution = attribution;
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
    /// Set on a release whose leaf's before-PR or before-landing review
    /// settled into an evidence hold. The owner keeps the task in progress
    /// with `review_awaiting_evidence` as its latest decision, so receipt of
    /// the named evidence queues a fresh review. An owner that predates the
    /// field releases the task unheld.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_hold: Option<orbit_types::workflow::ReviewEvidenceHold>,
    /// [ORB-15570] Set with `evidence_hold` when the leaf held in its
    /// before-landing review, after its `pr_open`: the pull request it opened,
    /// which stays open. The owner keeps it on the held candidate, so the run
    /// that resumes that candidate closes it once its own is open. An owner
    /// that predates the field leaves it open until the task ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_pull_request: Option<ClaimPullRequest>,
    /// [ORB-14634] Set on a `transient` release whose leaf held its claim
    /// while the forge refused its push, and released it once its retry
    /// window closed. The forge, not the host or the crew, refused it, so the
    /// release excludes neither from the drain's window. An owner that
    /// predates the field keeps the task from that drain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge_hold: Option<orbit_types::workflow::ForgeUnavailableHold>,
    /// [ORB-16699] Review ledgers the executing host held for this claim.
    /// Settlement copies them onto the owner so `review-reset` can reach the
    /// lineage a claimed leaf exhausted. An owner that predates the field
    /// ignores it. Absent when the leaf held no ledger for the claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_review: Option<ClaimedReviewCarry>,
    /// The crew the leaf ran as, from its run's resolved crew. The follower
    /// fills it on a failure or release; the owner keeps it on the claim's
    /// settlement record as attribution only. An owner that predates the
    /// field ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// The model that crew resolved to on the leaf's run, beside
    /// [`Self::crew`]. An owner that predates the field ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew_model: Option<String>,
}

/// Review ledgers a claimed leaf carried home [ORB-16699].
///
/// The lineage key stays the string the follower wrote. The owner stores
/// that same key in its own workspace partition, which is what
/// `review-reset` looks up. `executor_data_root` is that host's Orbit global
/// directory and `executor_workspace_id` is the workspace `--workspace`
/// selects there, so the resume command can name both from any directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ClaimedReviewCarry {
    /// Machine that executed the leaf and holds the follower ledger partition.
    pub executor_machine_id: String,
    /// That machine's Orbit global data directory.
    pub executor_data_root: String,
    /// Logical workspace id `--workspace` resolves on that machine. Empty on
    /// a carry written before the field existed; the lineage-key prefix is
    /// the fallback, and it matches this id for a workspace `workspace init`
    /// bound.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub executor_workspace_id: String,
    pub ledgers: Vec<ReviewLedger>,
}

/// [ORB-15570] A pull request a claimed leaf opened, by number and head
/// branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimPullRequest {
    pub number: String,
    pub head: String,
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
    /// [ORB-15573] Set on a `provider` failure whose provider's content
    /// policy refused the task. The provider works on the executing host, so
    /// the drain keeps offering the crew; the owner holds the task back from
    /// every crew of that provider instead. An owner that predates the field
    /// releases the task unheld.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provider_refusal: bool,
    /// Set on a `transient` failure whose owner call lost to the owner's own
    /// lock-wait deadline ([`orbit_types::workflow::OWNER_BUSY_MARKER`]): the
    /// owner was reached and decided nothing. Neither the executing host nor
    /// its crew is at fault, so the release excludes neither for the drain's
    /// window, and the drain backs its claims off for a bounded time instead.
    /// The owner honours the mark only on a `transient` release and only for
    /// the task's first few such releases within its release-budget window:
    /// those neither count against the task's release budget nor hold the
    /// task from the drain, and later ones are ordinary `transient` releases.
    /// An owner that predates the field drops it and records an ordinary
    /// `transient` release.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owner_busy: bool,
}

impl ClaimFailure {
    /// Whether the executing drain stops running the leaf's crew for the rest
    /// of its window: its class [says so](orbit_types::workflow::ClaimFailureClass::excludes_crew)
    /// and it is neither a provider's content refusal nor a busy owner.
    #[must_use]
    pub fn excludes_crew(&self) -> bool {
        self.class.excludes_crew() && !self.provider_refusal && !self.owner_busy
    }

    /// Whether a release for this failure counts against the task's release
    /// budget: its class is [budgeted](orbit_types::workflow::ClaimFailureClass::budgeted)
    /// and it is neither a provider usage limit nor a busy owner.
    #[must_use]
    pub fn budgeted(&self) -> bool {
        self.class.budgeted() && !self.provider_limit && !self.owner_busy
    }

    /// Whether a failed claim with this failure counts toward its pull
    /// drain's failure breaker [ORB-15639]: every one but Orbit's refusal of
    /// the implementer's handoff ([`orbit_types::workflow::is_contract_refusal`]),
    /// which blocks the task for an operator to requeue but says nothing
    /// about the executing host.
    #[must_use]
    pub fn trips_breaker(&self) -> bool {
        !orbit_types::workflow::is_contract_refusal(&self.reason)
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
    /// [ORB-15570] The head branch of `pull_request` when it is not
    /// `branch`: a held candidate is kept from the branch its leaf published
    /// it to for evidence, while its pull request stays on the leaf's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_head: Option<String>,
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
    /// [ORB-15314] The review the leaf was inside when it was cancelled or
    /// interrupted, with where its reviewer's uncommitted repairs were kept
    /// and the findings its report held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_review: Option<InterruptedReview>,
    /// [ORB-16890] Why the owner's landing of this handed-off candidate
    /// stopped: a required check the candidate itself fails, with the
    /// failed job, its signature and a log excerpt. The next run repairs the
    /// candidate from it and publishes its own pull request, which replaces
    /// `pull_request`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landing_stop: Option<String>,
}

/// [ORB-15314] A review a leaf was cancelled or interrupted inside, before
/// it settled.
///
/// The executor commits the reviewer's uncommitted edits as one marked commit
/// on top of the reviewed head and keeps it at `repair_ref`: a ref on
/// `origin` when it could push it, and always in its own repository. A
/// release recorded before the leaf stopped names the ref the stop will
/// write, so `repair_commit` is absent; the resume resolves the ref and
/// checks the commit before it applies it. Without a `repair_ref` the
/// reviewer left nothing uncommitted, or it could not be kept. The owner
/// adds the verdict and findings from its copy of the attempt's report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterruptedReview {
    /// The review step that was running (`review`, `landing_review`, ...).
    pub step_id: String,
    /// The review attempt that was admitted.
    pub attempt_id: String,
    /// Where the marked repair commit is kept
    /// (`refs/orbit/candidates/<task>/<run>-review`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_ref: Option<String>,
    /// The marked repair commit, when the settlement was recorded after it
    /// was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_commit: Option<String>,
    /// The verdict of the attempt's latest report on the owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<orbit_types::workflow::ReviewVerdict>,
    /// The findings of that report, with their severity and disposition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<orbit_types::workflow::ReviewFinding>,
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
