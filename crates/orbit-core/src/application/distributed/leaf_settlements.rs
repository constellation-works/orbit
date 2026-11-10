//! The failure and release settlements claimed leaves sent this owner
//! [ORB-14439].
//!
//! A claimed leaf runs on a follower, so the owner's run history never holds
//! it; the settlement its claim recorded is the owner's only account of why
//! it ended. This is the read-only list of those settlements, for a failure
//! scan on the owner. Only settled claims appear: an in-flight attempt stays
//! on the operator's claim inspection. The agent scorecards also read typed
//! handoffs from it, for the crew and model each pulled attempt ran as.

use orbit_common::OrbitError;
use orbit_store::contracts::{
    ClaimInspection, ClaimSettlementKind, ExecutionClaimPhase, LeafRootCause, LeafRunEvidence,
};
use orbit_types::workflow::{BASELINE_RED_HOLD_EVENT, ClaimFailureClass, OrbitBuild};
use serde::Serialize;

use crate::runtime::upgrade_handover::UPGRADE_QUIESCE_ERROR_CODE;

/// Events a failure or release settlement leaves as the claim's last event
/// when it settles. Claims settled before [`ClaimInspection::settlement`]
/// existed are found by these.
const SETTLEMENT_EVENTS: &[&str] = &[
    "claim_failed",
    "claim_released",
    "claim_release_budget_exhausted",
    "review_awaiting_evidence",
    BASELINE_RED_HOLD_EVENT,
];

/// One claimed leaf's failure or release settlement as this owner recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeafSettlement {
    pub task_id: String,
    pub claim_id: String,
    /// The machine the leaf ran on.
    pub machine_id: String,
    pub machine_name: Option<String>,
    /// The follower drain that admitted the claim.
    pub drain_run_id: String,
    /// The leaf run, on the follower's store.
    pub leaf_run_id: Option<String>,
    /// The claim's phase now.
    pub phase: ExecutionClaimPhase,
    pub last_event: String,
    pub settled_at: String,
    /// `fail`, `release` or `handoff`; `None` for a claim settled before the
    /// owner recorded settlements.
    pub kind: Option<ClaimSettlementKind>,
    /// The settlement's evidence class, or `unrecorded` when a claim settled
    /// before the owner recorded settlements and its release kept no class.
    pub evidence: &'static str,
    pub failure_class: Option<ClaimFailureClass>,
    /// A before-landing review refusal awaiting a recorded decision. Its
    /// class remains `candidate` and its claim still blocks the task.
    pub review_decision_pending: bool,
    pub crew: Option<String>,
    /// The model the leaf's crew resolved to, when its follower reported it.
    pub crew_model: Option<String>,
    /// The leaf's reasoning effort, when its follower reported it: the wire
    /// name, or `null` for the provider default. Absent when unreported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<Option<String>>,
    pub failed_step_id: Option<String>,
    /// Bounded, and as the leaf reported it: unredacted.
    pub reason: Option<String>,
    /// The leaf's own run record: job, build and failing step.
    pub run_evidence: LeafRunEvidenceView,
    /// How a failure scan accounts for this settlement [ORB-21840]. Derived
    /// from the typed fields above only, never from `reason`; it changes
    /// nothing about the claim or its task.
    pub accounting: SettlementAccounting,
}

/// What a settlement counts as in failure accounting [ORB-21840]: a failure,
/// or one of the designed waits and cancellations the owner's own run
/// history does not record as failed. Anything not proven to be one of those
/// counts as a failure, with the reason saying why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementOutcome {
    /// Counts in the failure rate.
    Failure,
    /// A before-landing review refusal awaiting a recorded decision: the
    /// claim still fails and blocks the task, and the pull request stays
    /// open and unmerged, but nothing broke.
    ReviewDecisionHold,
    /// A review released for evidence its host could not produce.
    EvidenceHold,
    /// An operator cancelled the launched leaf.
    OperatorCancel,
    /// An Orbit upgrade paused the leaf at a step boundary. Its claim is
    /// released for a later drain to re-admit: nothing broke.
    UpgradePaused,
    /// A required check failed on the base as well. It keeps its existing
    /// accounting: it counts as a failure.
    BaselineRed,
    /// Settled before the owner recorded settlements, with no typed class
    /// to read. Counted as a failure; the evidence is unknown.
    Unrecorded,
}

/// A settlement's [`SettlementOutcome`], whether it counts as a failure, and
/// the typed evidence that decided it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SettlementAccounting {
    pub outcome: SettlementOutcome,
    pub counts_as_failure: bool,
    /// A stable token naming the evidence: for example
    /// `before_landing_review_awaits_decision`, or
    /// `review_decision_pending_without_candidate_failure` when the pending
    /// flag sits on a settlement that cannot be that hold.
    pub reason: &'static str,
}

impl SettlementAccounting {
    const fn of(outcome: SettlementOutcome, reason: &'static str) -> Self {
        let counts_as_failure = matches!(
            outcome,
            SettlementOutcome::Failure
                | SettlementOutcome::BaselineRed
                | SettlementOutcome::Unrecorded
        );
        Self {
            outcome,
            counts_as_failure,
            reason,
        }
    }

    /// The accounting of a projected settlement, from its typed fields.
    fn derive(settlement: &LeafSettlement) -> Self {
        use SettlementOutcome as Outcome;
        let class = settlement.failure_class;
        if class == Some(ClaimFailureClass::OperatorCancel) {
            return Self::of(Outcome::OperatorCancel, "operator_cancel");
        }
        if settlement.review_decision_pending {
            // The flag the follower set from the review gate's typed
            // before-landing standing. Only a failed candidate settlement can
            // be that hold; anything else is malformed and stays a failure.
            return if settlement.kind == Some(ClaimSettlementKind::Fail)
                && class == Some(ClaimFailureClass::Candidate)
            {
                Self::of(
                    Outcome::ReviewDecisionHold,
                    "before_landing_review_awaits_decision",
                )
            } else {
                Self::of(
                    Outcome::Failure,
                    "review_decision_pending_without_candidate_failure",
                )
            };
        }
        match settlement.evidence {
            "evidence_hold" if settlement.kind != Some(ClaimSettlementKind::Fail) => {
                Self::of(Outcome::EvidenceHold, "evidence_hold")
            }
            "baseline_red" => Self::of(Outcome::BaselineRed, "baseline_red"),
            "unrecorded" => Self::of(Outcome::Unrecorded, "unrecorded"),
            // The leaf's own recorded step code, on the class an interrupted
            // leaf settles under. A follower that sent no run record, or any
            // other interruption, stays a failure.
            _ if class == Some(ClaimFailureClass::Transient)
                && settlement
                    .run_evidence
                    .root_cause
                    .as_ref()
                    .and_then(|cause| cause.code.as_deref())
                    == Some(UPGRADE_QUIESCE_ERROR_CODE) =>
            {
                Self::of(Outcome::UpgradePaused, UPGRADE_QUIESCE_ERROR_CODE)
            }
            _ if settlement.kind.is_none() => Self::of(Outcome::Failure, "legacy_release_class"),
            _ if class == Some(ClaimFailureClass::Candidate) => {
                Self::of(Outcome::Failure, "candidate_failure")
            }
            _ => Self::of(Outcome::Failure, "settlement_failure"),
        }
    }
}

/// What the owner holds of a settled leaf's own run record [ORB-20906]: its
/// job, build and failing step, from the evidence row the claim kept beside
/// its settlement. A claim with no such row has none of them, and each state
/// says so: `unrecorded` is never a guess from the owner's or any current
/// build, and never read out of the settlement's text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeafRunEvidenceView {
    /// `settlement` when the follower sent it with the settlement,
    /// `hydrated` when an operator observed it afterwards, or `None` when the
    /// claim holds no leaf-run evidence: it settled before the evidence
    /// existed, or its follower sent none, and no operator hydrated it.
    pub provenance: Option<&'static str>,
    /// When the owner recorded the evidence.
    pub recorded_at: Option<String>,
    pub job_id: Option<String>,
    /// The build the leaf's run record held.
    pub build: Option<OrbitBuild>,
    /// `recorded`, or `unrecorded` when the evidence holds no build or the
    /// claim holds no evidence at all.
    pub build_state: &'static str,
    /// The failing step the leaf's run record held.
    pub root_cause: Option<LeafRootCause>,
    /// `recorded`, or `unrecorded` like [`Self::build_state`].
    pub root_cause_state: &'static str,
}

impl LeafRunEvidenceView {
    fn of(evidence: Option<LeafRunEvidence>) -> Self {
        let Some(evidence) = evidence else {
            return Self {
                provenance: None,
                recorded_at: None,
                job_id: None,
                build: None,
                build_state: "unrecorded",
                root_cause: None,
                root_cause_state: "unrecorded",
            };
        };
        let state = |recorded: bool| if recorded { "recorded" } else { "unrecorded" };
        Self {
            provenance: Some(evidence.provenance.as_str()),
            recorded_at: Some(evidence.recorded_at),
            job_id: Some(evidence.report.job_id),
            build_state: state(evidence.report.build.is_some()),
            build: evidence.report.build,
            root_cause_state: state(evidence.report.root_cause.is_some()),
            root_cause: evidence.report.root_cause,
        }
    }
}

impl LeafSettlement {
    fn of(mut inspection: ClaimInspection) -> Option<Self> {
        let claim = &inspection.claim;
        let mut settlement = Self {
            task_id: claim.task_id.clone(),
            claim_id: claim.claim_id.clone(),
            machine_id: claim.executed_on.machine_id.clone(),
            machine_name: claim.executed_on.machine_name.clone(),
            drain_run_id: claim.run_context.run_id.clone(),
            leaf_run_id: inspection.bound_run.as_ref().map(|run| run.run_id.clone()),
            phase: claim.phase,
            last_event: inspection.last_event.clone(),
            settled_at: inspection.updated_at.clone(),
            kind: None,
            evidence: "unrecorded",
            failure_class: None,
            review_decision_pending: false,
            crew: None,
            crew_model: None,
            reasoning_effort: None,
            failed_step_id: None,
            reason: None,
            run_evidence: LeafRunEvidenceView::of(inspection.leaf_run_evidence.take()),
            accounting: SettlementAccounting::of(SettlementOutcome::Unrecorded, "unrecorded"),
        };
        if let Some(record) = inspection.settlement {
            settlement.settled_at = record.settled_at;
            settlement.kind = Some(record.kind);
            settlement.evidence = record.evidence.as_str();
            settlement.failure_class = record.failure_class;
            settlement.review_decision_pending = record.review_decision_pending;
            settlement.crew = record.crew;
            settlement.crew_model = record.crew_model;
            settlement.reasoning_effort = record.reasoning_effort;
            settlement.failed_step_id = record.failed_step_id;
            settlement.reason = Some(record.reason);
            settlement.accounting = SettlementAccounting::derive(&settlement);
            return Some(settlement);
        }
        if !SETTLEMENT_EVENTS.contains(&inspection.last_event.as_str()) {
            return None;
        }
        // A claim settled before the record existed keeps only its typed
        // release and its last event.
        if let Some(release) = inspection.release {
            settlement.settled_at = release.released_at;
            settlement.failure_class = Some(release.class);
            settlement.reason = Some(release.reason);
            settlement.evidence = if release.forge_unavailable {
                "forge_unavailable"
            } else {
                match release.class {
                    ClaimFailureClass::Provider => "provider_unavailable",
                    ClaimFailureClass::BaselineRed => "baseline_red",
                    _ => "failure",
                }
            };
        }
        match settlement.last_event.as_str() {
            "review_awaiting_evidence" => settlement.evidence = "evidence_hold",
            event if event == BASELINE_RED_HOLD_EVENT => settlement.evidence = "baseline_red",
            _ => {}
        }
        settlement.accounting = SettlementAccounting::derive(&settlement);
        Some(settlement)
    }
}

impl crate::OrbitRuntime {
    /// Every failure or release settlement a claimed leaf sent this owner,
    /// newest first, settled at or after `since` when given. `reconcile`
    /// first recovers an interrupted coordination commit; without it the
    /// read is strictly read-only and refuses while one is pending.
    pub fn leaf_settlements(
        &self,
        since: Option<chrono::DateTime<chrono::Utc>>,
        reconcile: bool,
    ) -> Result<Vec<LeafSettlement>, OrbitError> {
        let mut settlements = self
            .settled_leaves(reconcile)?
            .into_iter()
            .filter(|settlement| settlement.kind != Some(ClaimSettlementKind::Handoff))
            .filter(|settlement| {
                since.is_none_or(|since| {
                    chrono::DateTime::parse_from_rfc3339(&settlement.settled_at)
                        .is_ok_and(|settled| settled >= since)
                })
            })
            .collect::<Vec<_>>();
        // Claim id breaks settled-at ties so the order is deterministic
        // across calls; hydration continuation pages through this order.
        settlements.sort_by(|a, b| {
            b.settled_at
                .cmp(&a.settled_at)
                .then_with(|| a.claim_id.cmp(&b.claim_id))
        });
        Ok(settlements)
    }

    /// Every settlement a claimed leaf sent this owner, typed handoffs
    /// included, in no order. Strictly read-only unless `reconcile`.
    pub(crate) fn settled_leaves(
        &self,
        reconcile: bool,
    ) -> Result<Vec<LeafSettlement>, OrbitError> {
        let claims = if reconcile {
            self.resolve_execution_claims()?
        } else {
            self.inspect_execution_claims()?
        };
        Ok(claims.into_iter().filter_map(LeafSettlement::of).collect())
    }
}
