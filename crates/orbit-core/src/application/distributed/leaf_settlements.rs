//! The failure and release settlements claimed leaves sent this owner
//! [ORB-14439].
//!
//! A claimed leaf runs on a follower, so the owner's run history never holds
//! it; the settlement its claim recorded is the owner's only account of why
//! it ended. This is the read-only list of those settlements, for a failure
//! scan on the owner. Only settled claims appear: an in-flight attempt stays
//! on the operator's claim inspection.

use orbit_common::OrbitError;
use orbit_store::contracts::{ClaimInspection, ClaimSettlementKind, ExecutionClaimPhase};
use orbit_types::workflow::{BASELINE_RED_HOLD_EVENT, ClaimFailureClass};
use serde::Serialize;

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
    /// `fail` or `release`; `None` for a claim settled before the owner
    /// recorded settlements.
    pub kind: Option<ClaimSettlementKind>,
    /// The settlement's evidence class, or `unrecorded` when a claim settled
    /// before the owner recorded settlements and its release kept no class.
    pub evidence: &'static str,
    pub failure_class: Option<ClaimFailureClass>,
    pub crew: Option<String>,
    pub failed_step_id: Option<String>,
    /// Bounded, and as the leaf reported it: unredacted.
    pub reason: Option<String>,
}

impl LeafSettlement {
    fn of(inspection: ClaimInspection) -> Option<Self> {
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
            crew: None,
            failed_step_id: None,
            reason: None,
        };
        if let Some(record) = inspection.settlement {
            settlement.settled_at = record.settled_at;
            settlement.kind = Some(record.kind);
            settlement.evidence = record.evidence.as_str();
            settlement.failure_class = record.failure_class;
            settlement.crew = record.crew;
            settlement.failed_step_id = record.failed_step_id;
            settlement.reason = Some(record.reason);
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
        let claims = if reconcile {
            self.resolve_execution_claims()?
        } else {
            self.inspect_execution_claims()?
        };
        let mut settlements = claims
            .into_iter()
            .filter_map(LeafSettlement::of)
            .filter(|settlement| {
                since.is_none_or(|since| {
                    chrono::DateTime::parse_from_rfc3339(&settlement.settled_at)
                        .is_ok_and(|settled| settled >= since)
                })
            })
            .collect::<Vec<_>>();
        settlements.sort_by(|a, b| b.settled_at.cmp(&a.settled_at));
        Ok(settlements)
    }
}
