//! The decision for each landing mutation of a handed-off claim: retries,
//! dispatch, waits, history, completion, external merges, stops and merge
//! intents.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{TaskDecision, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::handoff::LandingRetryRequest;
use orbit_types::workflow::{PrBaseUpdateReservation, handoff};

use super::super::super::TaskCommitBoundary;
use super::super::super::landing::LandingStop;
use super::super::{encode, invalid};
use super::decide::ClaimDecision;
use crate::contract::*;

/// What an operator reports of a pull request merged outside Orbit.
pub(super) struct ExternalMerge<'a> {
    pub(super) pull_request: u64,
    pub(super) base_branch: &'a str,
    pub(super) merge_commit: &'a str,
    pub(super) note: &'a str,
    pub(super) verified_candidate_head: Option<&'a str>,
}

/// A landing history event an operator asks the claim to record.
pub(super) struct LandingHistoryRecord<'a> {
    pub(super) event: &'a str,
    pub(super) note: &'a str,
    pub(super) decision: Option<&'a TaskDecision>,
}

/// Landing events an operator may record as task history.
const RECORDABLE_LANDING_EVENTS: [&str; 5] = [
    "ci_check_rerun",
    "ci_base_suspect",
    "ci_check_rerun_reserved",
    "ci_infrastructure_retry_backoff",
    "ci_infrastructure_retry_exhausted",
];

fn recordable_landing_event(event: &str) -> bool {
    RECORDABLE_LANDING_EVENTS.contains(&event)
        || [
            orbit_types::workflow::CI_FAILURE_RECORDED_EVENT,
            orbit_types::workflow::CI_FAILURE_RECLASSIFIED_EVENT,
            orbit_types::workflow::CI_BASE_SUSPECT_RECLASSIFIED_EVENT,
        ]
        .contains(&event)
}

/// Landing mutations that record or wait act only for an operator on a
/// handed-off claim whose landing context is still current.
fn require_landing_context(
    auth: &ClaimInvocation,
    decision: &ClaimDecision,
) -> Result<(), OrbitError> {
    if !auth.operator
        || decision.claim.phase != ExecutionClaimPhase::HandedOff
        || decision.state.landing_invalidated
    {
        return Err(invalid("current operator landing context required"));
    }
    Ok(())
}

impl ClaimDecision {
    /// A landing stop ends the claim in the phase the stop names. A stop that
    /// keeps its candidate leaves it for the task's next claim [ORB-16890].
    fn apply_landing_stop(&mut self, stop: LandingStop) {
        let kept = match stop {
            LandingStop::Stopped => None,
            LandingStop::Repair => {
                self.state.claim.phase = ExecutionClaimPhase::RepairPending;
                self.state.landing_invalidated = true;
                None
            }
            LandingStop::Blocked(comment) => {
                self.state.claim.phase = ExecutionClaimPhase::Failed;
                self.state.landing_invalidated = true;
                self.evidence.comment = Some(comment);
                self.release = true;
                None
            }
            // The claim ends like a release: the task's next claim or owner
            // run is offered the kept candidate.
            LandingStop::Requeue(kept) => {
                self.state.claim.phase = ExecutionClaimPhase::Revoked;
                self.state.landing_invalidated = true;
                self.release = true;
                self.state.last_event = handoff::LANDING_REPAIR_REQUEUED_EVENT.into();
                Some(*kept)
            }
            LandingStop::RepairBudgetExhausted { comment, kept } => {
                self.state.claim.phase = ExecutionClaimPhase::Failed;
                self.state.landing_invalidated = true;
                self.evidence.comment = Some(comment);
                self.release = true;
                self.state.last_event = handoff::LANDING_REPAIR_EXHAUSTED_EVENT.into();
                Some(*kept)
            }
        };
        if let Some(kept) = kept {
            self.state.preserved_candidate = Some(kept);
            self.params
                .append_history
                .extend(TaskCommitBoundary::kept_candidate_entry(
                    &self.claim,
                    self.state.preserved_candidate.as_ref(),
                ));
        }
    }
}

impl TaskCommitBoundary {
    pub(super) fn decide_attach_landing_retry(
        &self,
        auth: &ClaimInvocation,
        handoff_id: &str,
        request_key: &str,
        job_run_id: &str,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        let ClaimDecision {
            state,
            params,
            handoff_effects,
            ..
        } = decision;
        self.attach_landing_retry(
            auth,
            state,
            handoff_id,
            request_key,
            job_run_id,
            params,
            handoff_effects,
        )?;
        state.last_event = "landing_dispatched".into();
        Ok(())
    }

    pub(super) fn decide_reserve_base_update(
        &self,
        auth: &ClaimInvocation,
        request: &PrBaseUpdateReservation,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        use orbit_types::workflow::{
            PR_BASE_UPDATE_RESERVED_EVENT, PrBaseUpdateAdmission, decide_pr_base_update,
        };
        let ClaimDecision {
            state,
            bundle,
            params,
            ..
        } = decision;
        self.check_base_update_scope(auth, state, request)?;
        let history = crate::boundary::v2::task_history_from_events(bundle.events.clone());
        if !matches!(
            decide_pr_base_update(&history, request),
            PrBaseUpdateAdmission::Reserved
        ) {
            return Err(invalid("the cumulative base update is already reserved"));
        }
        params.append_history.push(
            TaskHistoryEntry::new(
                Utc::now(),
                auth.machine_id.clone(),
                PR_BASE_UPDATE_RESERVED_EVENT,
            )
            .with_note(Some(encode(request)?)),
        );
        state.last_event = "handoff_base_update_admitted".into();
        Ok(())
    }

    pub(super) fn decide_retry_landing(
        &self,
        auth: &ClaimInvocation,
        request: &LandingRetryRequest,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        let ClaimDecision {
            state,
            params,
            handoff_effects,
            ..
        } = decision;
        self.reserve_landing_retry(auth, state, request, params, handoff_effects)?;
        state.last_event = "landing_retry_reserved".into();
        Ok(())
    }

    pub(super) fn decide_dispatch_landing(
        &self,
        auth: &ClaimInvocation,
        handoff_id: &str,
        job_run_id: Option<&String>,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        let ClaimDecision {
            state,
            params,
            handoff_effects,
            ..
        } = decision;
        self.dispatch_landing_attempt(
            auth,
            state,
            handoff_id,
            job_run_id,
            params,
            handoff_effects,
        )?;
        state.last_event = "landing_dispatched".into();
        Ok(())
    }

    pub(super) fn decide_landing_wait(
        &self,
        auth: &ClaimInvocation,
        handoff_id: &str,
        reason: &str,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        require_landing_context(auth, decision)?;
        if reason.trim().is_empty() {
            return Err(invalid("a landing wait requires a reason"));
        }
        self.check_live_landing_attempt(auth, handoff_id)?;
        decision.params.status_note = Some(reason.to_string());
        decision.state.last_event = handoff::LANDING_WAITING_FOR_BASE_EVENT.into();
        Ok(())
    }

    pub(super) fn decide_landing_history(
        &self,
        auth: &ClaimInvocation,
        handoff_id: &str,
        record: LandingHistoryRecord<'_>,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        require_landing_context(auth, decision)?;
        validate_landing_history(&record)?;
        self.check_live_landing_attempt(auth, handoff_id)?;
        decision.params.append_history.push(
            TaskHistoryEntry::new(Utc::now(), "system", record.event)
                .with_note(Some(record.note.to_string()))
                .with_decision(record.decision.cloned()),
        );
        decision.history_event = false;
        Ok(())
    }

    pub(super) fn decide_landing_completion_hold(
        &self,
        auth: &ClaimInvocation,
        handoff_id: &str,
        history: &TaskHistoryEntry,
        comment: &str,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        require_landing_context(auth, decision)?;
        if history.event != orbit_types::workflow::COMPLETION_HELD_EVENT {
            return Err(invalid("unsupported landing completion hold event"));
        }
        if comment.trim().is_empty() {
            return Err(invalid("landing completion hold requires a comment"));
        }
        self.check_live_landing_attempt(auth, handoff_id)?;
        decision.params.append_history.push(history.clone());
        decision.landing_comments.push(comment.to_string());
        decision.history_event = false;
        Ok(())
    }

    pub(super) fn decide_complete_landing(
        &self,
        auth: &ClaimInvocation,
        handoff_id: &str,
        proof: &str,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        let ClaimDecision {
            state,
            bundle,
            params,
            handoff_effects,
            ..
        } = decision;
        self.complete_landing_attempt(
            auth,
            state,
            handoff_id,
            proof,
            &crate::boundary::v2::task_history_from_events(bundle.events.clone()),
            params,
            handoff_effects,
        )?;
        state.claim.phase = ExecutionClaimPhase::Landed;
        state.last_event = "landing_completed".into();
        Ok(())
    }

    pub(super) fn decide_settle_external_merge(
        &self,
        auth: &ClaimInvocation,
        handoff_id: &str,
        merge: ExternalMerge<'_>,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        let ClaimDecision {
            state,
            bundle,
            params,
            release,
            handoff_effects,
            ..
        } = decision;
        if merge.note.trim().is_empty() {
            return Err(invalid("external merge settlement requires a landing note"));
        }
        self.check_external_merge_settlement(
            auth,
            state,
            handoff_id,
            merge.pull_request,
            merge.base_branch,
            merge.merge_commit,
            merge.verified_candidate_head,
            &crate::boundary::v2::task_history_from_events(bundle.events.clone()),
        )?;
        if self.handoff_authority_revoked(handoff_id)? {
            state.claim.phase = ExecutionClaimPhase::Revoked;
            state.landing_invalidated = true;
        } else {
            self.complete_external_merge_attempt(handoff_id, merge.note, handoff_effects)?;
            state.claim.phase = ExecutionClaimPhase::Landed;
        }
        *release = true;
        params.status = Some(TaskStatus::Done);
        params.status_note = Some(merge.note.to_string());
        handoff_effects.worker_update = Some(ClaimWorkerUpdate {
            external_refs: vec![orbit_types::task::ExternalRef::github_pr(
                merge.pull_request.to_string(),
            )?],
            ..ClaimWorkerUpdate::default()
        });
        state.last_event = handoff::EXTERNAL_MERGE_SETTLED_EVENT.into();
        Ok(())
    }

    pub(super) fn decide_stop_landing(
        &self,
        auth: &ClaimInvocation,
        handoff_id: &str,
        reason: &str,
        repairable: bool,
        requeue: bool,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        decision.state.last_event = "landing_stopped".into();
        let stop = self.stop_landing_attempt(
            auth,
            &decision.state,
            handoff_id,
            reason,
            repairable,
            requeue,
            &mut decision.params,
            &mut decision.handoff_effects,
        )?;
        decision.apply_landing_stop(stop);
        Ok(())
    }

    pub(super) fn decide_merge_intent(
        &self,
        auth: &ClaimInvocation,
        intent_id: &str,
        resolved: bool,
        proof: &str,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        require_landing_context(auth, decision)?;
        if intent_id.trim().is_empty() || proof.trim().is_empty() {
            return Err(invalid("merge intent evidence required"));
        }
        let ClaimDecision { state, params, .. } = decision;
        if resolved {
            if state.unresolved_merge_intent.as_deref() != Some(intent_id) {
                return Err(invalid("merge intent mismatch"));
            }
            state.unresolved_merge_intent = None;
        } else {
            if state.unresolved_merge_intent.is_some() {
                return Err(invalid("merge intent already unresolved"));
            }
            self.check_handoff_landing(auth, state)?;
            state.unresolved_merge_intent = Some(intent_id.to_string());
        }
        params.status_note = Some(encode(&(intent_id, resolved, proof))?);
        state.last_event = "claim_merge_intent".into();
        Ok(())
    }
}

/// A landing history record names an event the claim may record, a note, and
/// at most the CI rerun reservation its note spells out.
fn validate_landing_history(record: &LandingHistoryRecord<'_>) -> Result<(), OrbitError> {
    if !recordable_landing_event(record.event) {
        return Err(invalid("unsupported handoff landing history event"));
    }
    if record.note.trim().is_empty() {
        return Err(invalid("handoff landing history requires a note"));
    }
    if let Some(decision) = record.decision {
        let TaskDecision::CiRerunReserved(plan) = decision else {
            return Err(invalid("unsupported handoff landing history decision"));
        };
        if record.event != orbit_types::workflow::CI_RERUN_RESERVED_EVENT
            || record.note != plan.reservation_note()
        {
            return Err(invalid("handoff landing history decision mismatch"));
        }
    }
    Ok(())
}
