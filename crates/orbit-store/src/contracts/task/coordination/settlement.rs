//! Claim settlement records and lifecycle mutation contracts.

use orbit_types::task::TaskStatus;
use serde::{Deserialize, Serialize};

use super::{
    AdmissionShipContract, ClaimEvidence, ClaimRun, ExecutionClaim, ExecutionClaimPhase,
    ExecutionLocation, PreservedClaimCandidate, TaskCoordinationRow,
};

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
    /// [ORB-14634] The release was for a forge outage
    /// ([`ClaimEvidence::forge_hold`]), which blames neither the drain's host
    /// nor its crew: admission does not keep the task from that drain.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forge_unavailable: bool,
    /// [ORB-14695] The release was for a provider usage limit
    /// (`ClaimFailure::provider_limit`), which does not count against the
    /// task's release budget.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provider_limit: bool,
}

impl ClaimReleaseRecord {
    /// Whether this release counts against the task's release budget.
    #[must_use]
    pub fn budgeted(&self) -> bool {
        self.class.budgeted() && !self.provider_limit
    }
}

/// What a claimed leaf's failure or release settlement told the owner, kept
/// on the claim's lifecycle state [ORB-14439]. The leaf ran on another host,
/// so the owner's run history never records it; this is the owner's only
/// durable account of why it ended. Claims settled before the record existed
/// carry none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimSettlementRecord {
    /// The settlement the leaf sent. A release the owner turned into a
    /// failure still reads `release`; the claim's phase says what applied.
    pub kind: ClaimSettlementKind,
    /// The most specific evidence the settlement carried.
    pub evidence: ClaimSettlementEvidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<orbit_types::workflow::ClaimFailureClass>,
    /// The typed failure's reason, else the settlement summary; bounded.
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// The first leaf step that did not complete, when the leaf kept a
    /// candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_step_id: Option<String>,
    pub settled_at: String,
}

/// Largest reason a [`ClaimSettlementRecord`] keeps.
const MAX_SETTLEMENT_REASON_BYTES: usize = 1024;

impl ClaimSettlementRecord {
    #[must_use]
    pub fn of(kind: ClaimSettlementKind, evidence: &ClaimEvidence, settled_at: String) -> Self {
        let failure = evidence.failure.as_ref();
        let reason = failure
            .map(|failure| failure.reason.as_str())
            .or(evidence.summary.as_deref())
            .unwrap_or_default()
            .trim();
        let cut = orbit_common::text::floor_char_boundary(reason, MAX_SETTLEMENT_REASON_BYTES);
        Self {
            kind,
            evidence: ClaimSettlementEvidence::of(evidence),
            failure_class: failure.map(|failure| failure.class),
            reason: reason[..cut].to_string(),
            crew: failure
                .and_then(|failure| failure.crew.clone())
                .or_else(|| {
                    evidence
                        .provider_unavailable
                        .as_ref()
                        .and_then(|provider| provider.crew.clone())
                }),
            failed_step_id: failure
                .and_then(|failure| failure.candidate.as_ref())
                .and_then(|candidate| candidate.failed_step_id.clone()),
            settled_at,
        }
    }
}

/// Which settlement a claimed leaf sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimSettlementKind {
    /// The leaf failed its claim; the owner blocks the task.
    Fail,
    /// The leaf gave its claim back unfinished.
    Release,
}

/// The evidence class of a claimed leaf's settlement, most specific first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimSettlementEvidence {
    /// [`ClaimEvidence::provider_unavailable`].
    ProviderUnavailable,
    /// [`ClaimEvidence::baseline_red`].
    BaselineRed,
    /// [`ClaimEvidence::forge_hold`].
    ForgeUnavailable,
    /// [`ClaimEvidence::evidence_hold`].
    EvidenceHold,
    /// [`ClaimEvidence::final_recovery`].
    FinalRecovery,
    /// Only a typed [`ClaimEvidence::failure`].
    Failure,
    /// Only a summary: an untyped settlement, such as a leaf that never
    /// launched because the owner refused its bind.
    Summary,
}

impl ClaimSettlementEvidence {
    #[must_use]
    pub fn of(evidence: &ClaimEvidence) -> Self {
        if evidence.provider_unavailable.is_some() {
            Self::ProviderUnavailable
        } else if evidence.baseline_red.is_some() {
            Self::BaselineRed
        } else if evidence.forge_hold.is_some() {
            Self::ForgeUnavailable
        } else if evidence.evidence_hold.is_some() {
            Self::EvidenceHold
        } else if evidence.final_recovery.is_some() {
            Self::FinalRecovery
        } else if evidence.failure.is_some() {
            Self::Failure
        } else {
            Self::Summary
        }
    }

    /// The class's wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderUnavailable => "provider_unavailable",
            Self::BaselineRed => "baseline_red",
            Self::ForgeUnavailable => "forge_unavailable",
            Self::EvidenceHold => "evidence_hold",
            Self::FinalRecovery => "final_recovery",
            Self::Failure => "failure",
            Self::Summary => "summary",
        }
    }
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
    Friction(crate::contracts::FrictionAddParams),
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
    /// [ORB-14439] The failure or release settlement the claim's leaf sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement: Option<ClaimSettlementRecord>,
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
    pub friction: Option<(crate::contracts::FrictionAddParams, String)>,
    pub execution_origin: Option<ExecutionLocation>,
    pub worker_update: Option<ClaimWorkerUpdate>,
}
