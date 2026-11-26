//! The decision phase of a claim mutation: what the mutation does to the
//! claim's state, the task commit and its effects, before anything commits.
//! Release and landing mutations have their own modules.

use chrono::Utc;
use orbit_common::{ClaimRefusalKind, OrbitError};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::handoff::TaskHandoff;

use super::super::super::TaskCommitBoundary;
use super::super::codec::{HANDOFF_SETTLEMENT, LEAF_RUN_EVIDENCE};
use super::super::{decode_state, invalid, row};
use super::landing::{ExternalMerge, LandingHistoryRecord};
use crate::boundary::v2_bundle::TaskBundleV2;
use crate::contract::*;

/// What a claim mutation has decided so far. The decision phase fills it in,
/// and the commit phase reads it.
pub(super) struct ClaimDecision {
    pub(super) claim: ExecutionClaim,
    pub(super) bundle: TaskBundleV2,
    pub(super) expected_status: TaskStatus,
    pub(super) state: ClaimInspection,
    pub(super) params: TaskCoordinationCommitParams,
    pub(super) evidence: ClaimEvidence,
    pub(super) binding: Option<ClaimRun>,
    pub(super) release: bool,
    /// A steering snapshot or a landing history record changes no task
    /// status, so it writes no task history event.
    pub(super) history_event: bool,
    /// System comments a landing completion hold writes beside its history.
    pub(super) landing_comments: Vec<String>,
    pub(super) handoff_effects: ClaimCommitEffects,
}

impl ClaimDecision {
    pub(super) fn new(
        auth: &ClaimInvocation,
        claim: ExecutionClaim,
        state: ClaimInspection,
        bundle: TaskBundleV2,
        expected_status: TaskStatus,
    ) -> Self {
        Self {
            params: TaskCoordinationCommitParams {
                task_id: claim.task_id.clone(),
                actor: auth.machine_id.clone(),
                expected_status: vec![expected_status],
                ..Default::default()
            },
            claim,
            bundle,
            expected_status,
            state,
            evidence: ClaimEvidence::default(),
            binding: None,
            release: false,
            history_event: true,
            landing_comments: Vec::new(),
            handoff_effects: ClaimCommitEffects::default(),
        }
    }
}

pub(super) fn stale_claim() -> OrbitError {
    OrbitError::claim_refused(ClaimRefusalKind::StaleClaim)
}

impl TaskCommitBoundary {
    /// Decide what `mutation` does, one arm per mutation kind.
    pub(super) fn decide_claim_mutation(
        &self,
        auth: &ClaimInvocation,
        mutation: &ClaimMutation,
        receipt_id: &str,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        match mutation {
            ClaimMutation::Steering(record) => decide_steering(auth, record, decision),
            ClaimMutation::Update(value) => decide_update(auth, value, decision),
            ClaimMutation::Friction(value) => decide_friction(auth, value, receipt_id, decision),
            ClaimMutation::Bind { run, ship } => self.decide_bind(auth, run, ship, decision),
            ClaimMutation::Handoff(_) => Err(invalid("typed handoff required")),
            ClaimMutation::AcceptHandoff(handoff) => {
                self.decide_accept_handoff(auth, handoff, decision)
            }
            ClaimMutation::ApproveHandoff {
                handoff_id,
                candidate,
            } => {
                let ClaimDecision { state, params, .. } = decision;
                self.approve_typed_handoff(auth, state, handoff_id, candidate, params)?;
                state.last_event = "handoff_approved".into();
                Ok(())
            }
            ClaimMutation::RevokeHandoff { handoff_id, reason } => {
                self.decide_revoke_handoff(auth, handoff_id, reason, decision)
            }
            ClaimMutation::Evidence(value) => decide_evidence(auth, value, decision),
            ClaimMutation::Fail(value) => {
                decide_evidence(auth, value, decision)?;
                self.decide_fail(auth, value, decision)
            }
            ClaimMutation::Release(value) => self.decide_release(auth, value, decision),
            ClaimMutation::Recover {
                status,
                reason,
                expected_phase,
            } => self.decide_recover(auth, *status, reason, *expected_phase, decision),
            ClaimMutation::AttachLandingRetry {
                handoff_id,
                request_key,
                job_run_id,
            } => self.decide_attach_landing_retry(
                auth,
                handoff_id,
                request_key,
                job_run_id,
                decision,
            ),
            ClaimMutation::ReserveBaseUpdate(request) => {
                self.decide_reserve_base_update(auth, request, decision)
            }
            ClaimMutation::RetryLanding(request) => {
                self.decide_retry_landing(auth, request, decision)
            }
            ClaimMutation::DispatchLanding {
                handoff_id,
                job_run_id,
            } => self.decide_dispatch_landing(auth, handoff_id, job_run_id.as_ref(), decision),
            ClaimMutation::RecordLandingWait { handoff_id, reason } => {
                self.decide_landing_wait(auth, handoff_id, reason, decision)
            }
            ClaimMutation::RecordLandingHistory {
                handoff_id,
                event,
                note,
                decision: recorded,
            } => self.decide_landing_history(
                auth,
                handoff_id,
                LandingHistoryRecord {
                    event,
                    note,
                    decision: recorded.as_ref(),
                },
                decision,
            ),
            ClaimMutation::RecordLandingCompletionHold {
                handoff_id,
                history,
                comment,
            } => self.decide_landing_completion_hold(auth, handoff_id, history, comment, decision),
            ClaimMutation::CompleteLanding {
                handoff_id,
                evidence,
            } => self.decide_complete_landing(auth, handoff_id, evidence, decision),
            ClaimMutation::SettleExternalMerge {
                handoff_id,
                pull_request,
                base_branch,
                merge_commit,
                note,
                verified_candidate_head,
            } => self.decide_settle_external_merge(
                auth,
                handoff_id,
                ExternalMerge {
                    pull_request: *pull_request,
                    base_branch,
                    merge_commit,
                    note,
                    verified_candidate_head: verified_candidate_head.as_deref(),
                },
                decision,
            ),
            ClaimMutation::StopLanding {
                handoff_id,
                reason,
                repairable,
                requeue,
            } => {
                self.decide_stop_landing(auth, handoff_id, reason, *repairable, *requeue, decision)
            }
            ClaimMutation::MergeIntent {
                intent_id,
                resolved,
                evidence,
            } => self.decide_merge_intent(auth, intent_id, *resolved, evidence, decision),
        }
    }

    fn decide_bind(
        &self,
        auth: &ClaimInvocation,
        run: &ClaimRun,
        ship: &AdmissionShipContract,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        let ClaimDecision {
            claim,
            state,
            binding,
            ..
        } = decision;
        if auth.operator
            || claim.phase != ExecutionClaimPhase::Claimed
            || state.bound_run.is_some()
            || run.machine_id != auth.machine_id
            || run.run_id.trim().is_empty()
        {
            return Err(stale_claim());
        }
        let original = self.lookup_admission(
            &AdmissionIdentity::trusted_local(claim.executed_on.clone()),
            &claim.request_id,
        )?;
        let AdmissionLookup::Found { receipt, .. } = original else {
            return Err(invalid("claim receipt unavailable"));
        };
        if receipt.request.ship != *ship {
            return Err(invalid("claim policy mismatch"));
        }
        // One host-qualified leaf cannot belong to two attempts, even after settlement.
        if self
            .lifecycle_rows_for_bound_run(&run.run_id)?
            .iter()
            .any(|r| decode_state(&r.payload_json).is_ok_and(|s| s.bound_run.as_ref() == Some(run)))
        {
            return Err(invalid("leaf run already bound"));
        }
        *binding = Some(run.clone());
        state.bound_run = Some(run.clone());
        state.claim.phase = ExecutionClaimPhase::Running;
        state.last_event = "claim_bound".into();
        Ok(())
    }

    fn decide_accept_handoff(
        &self,
        auth: &ClaimInvocation,
        handoff: &TaskHandoff,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        let ClaimDecision {
            claim,
            state,
            params,
            evidence,
            release,
            handoff_effects,
            ..
        } = decision;
        state.claim.footprint =
            self.accept_typed_handoff(auth, state, handoff, params, handoff_effects)?;
        evidence.summary = Some(handoff.execution_summary.clone());
        // Attribution only: the record keeps the crew and model
        // the follower reported beside the handoff.
        // Older lifecycle readers have a closed fail/release enum.
        // Keep handoff attribution in its own immutable row, in the
        // same commit as authority, lifecycle and mutation receipt.
        params.rows.push(row(
            HANDOFF_SETTLEMENT,
            &claim.claim_id,
            &ClaimSettlementRecord::of(
                ClaimSettlementKind::Handoff,
                evidence,
                &auth.attribution,
                Utc::now().to_rfc3339(),
            ),
        )?);
        state.claim.phase = ExecutionClaimPhase::HandedOff;
        params.status = Some(TaskStatus::Review);
        *release = true;
        state.last_event = "claim_handed_off".into();
        Ok(())
    }

    fn decide_revoke_handoff(
        &self,
        auth: &ClaimInvocation,
        handoff_id: &str,
        reason: &str,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        let ClaimDecision {
            claim,
            state,
            params,
            handoff_effects,
            ..
        } = decision;
        if !auth.operator
            || claim.phase != ExecutionClaimPhase::HandedOff
            || reason.trim().is_empty()
        {
            return Err(invalid("operator handoff revocation requires a reason"));
        }
        if state.unresolved_merge_intent.is_some() {
            return Err(OrbitError::claim_refused(
                ClaimRefusalKind::UnresolvedMergeIntent,
            ));
        }
        if self.accepted_handoff(&auth.claim_id)?.handoff_id != handoff_id {
            return Err(OrbitError::claim_refused(
                ClaimRefusalKind::HandoffIdentityMismatch,
            ));
        }
        self.revoke_handoff_authority(auth, reason, params, handoff_effects)?;
        state.landing_invalidated = true;
        state.last_event = "handoff_revoked".into();
        params.status_note = Some(reason.to_string());
        Ok(())
    }

    fn decide_fail(
        &self,
        auth: &ClaimInvocation,
        value: &ClaimEvidence,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        let ClaimDecision {
            claim,
            state,
            params,
            release,
            ..
        } = decision;
        if value.summary.as_deref().is_none_or(|s| s.trim().is_empty()) {
            return Err(invalid("failure settlement requires evidence"));
        }
        state.claim.phase = ExecutionClaimPhase::Failed;
        state.landing_invalidated = true;
        params.status = Some(TaskStatus::Blocked);
        *release = true;
        state.last_event = "claim_failed".into();
        let settlement = ClaimSettlementRecord::of(
            ClaimSettlementKind::Fail,
            value,
            &auth.attribution,
            Utc::now().to_rfc3339(),
        );
        params.status_note = Some(
            ClaimFailedNote::of(&settlement, &claim.executed_on, state.bound_run.as_ref()).text(),
        );
        state.settlement = Some(settlement);
        params.rows.extend(leaf_run_evidence_row(
            claim,
            state,
            value.leaf_run.as_ref(),
        )?);
        state.preserved_candidate = self.preserve_candidate(&claim.task_id, value)?;
        params.append_history.extend(Self::kept_candidate_entry(
            claim,
            state.preserved_candidate.as_ref(),
        ));
        Ok(())
    }

    fn decide_recover(
        &self,
        auth: &ClaimInvocation,
        status: TaskStatus,
        reason: &str,
        expected_phase: ExecutionClaimPhase,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        let ClaimDecision {
            claim,
            expected_status,
            state,
            params,
            release,
            handoff_effects,
            ..
        } = decision;
        if !auth.operator {
            return Err(invalid("operator recovery capability required"));
        }
        // The phase precondition is checked here, inside the
        // transaction, rather than by the caller before the
        // mutation: an identical request whose receipt is
        // already recorded replays above without reaching this
        // arm, while a fresh request against a claim that has
        // moved on is refused.
        if claim.phase != expected_phase {
            return Err(stale_claim());
        }
        if state.unresolved_merge_intent.is_some() {
            return Err(OrbitError::claim_refused(
                ClaimRefusalKind::UnresolvedMergeIntent,
            ));
        }
        if reason.trim().is_empty() {
            return Err(invalid("recovery requires a reason"));
        }
        // The owner's lifecycle and completion gates chose the
        // target before this transaction; the store keeps the
        // invariants it can see: a fenced attempt never leaves
        // the task in progress or proposed, and `done` follows
        // only the review the claim pinned.
        if matches!(status, TaskStatus::InProgress | TaskStatus::Proposed)
            || (status == TaskStatus::Done && *expected_status != TaskStatus::Review)
        {
            return Err(invalid(&format!(
                "recovery cannot move a '{expected_status}' task to '{status}'"
            )));
        }
        self.revoke_handoff_authority(auth, reason, params, handoff_effects)?;
        state.claim.phase = ExecutionClaimPhase::Revoked;
        state.landing_invalidated = true;
        params.status = Some(status);
        params.status_note = Some(reason.to_string());
        *release = true;
        state.last_event = "claim_revoked".into();
        Ok(())
    }
}

fn decide_steering(
    auth: &ClaimInvocation,
    record: &ClaimSteeringRecord,
    decision: &mut ClaimDecision,
) -> Result<(), OrbitError> {
    let ClaimDecision {
        claim,
        state,
        params,
        history_event,
        ..
    } = decision;
    if auth.operator || claim.phase != ExecutionClaimPhase::Running {
        return Err(stale_claim());
    }
    let run_id = state
        .bound_run
        .as_ref()
        .map(|run| run.run_id.clone())
        .ok_or_else(|| invalid("claim binding is inconsistent with phase"))?;
    let watermark = state.steering.get_or_insert_with(Default::default);
    match record {
        ClaimSteeringRecord::Snapshot {
            step_id,
            newest,
            built_at,
        } => {
            watermark.record(step_id, *newest, *built_at);
            // A steering snapshot changes only the claim's own record, so it
            // writes no task history event.
            *history_event = false;
        }
        ClaimSteeringRecord::Delivery {
            step_id,
            through,
            delivered,
            truncated,
            built_at,
        } => {
            watermark.record_delivery(step_id, *through, delivered, *built_at);
            params.status_note = Some(
                serde_json::json!({
                    "run_id": run_id,
                    "step_id": step_id,
                    "delivered": delivered,
                    "truncated": truncated,
                    "claim_id": claim.claim_id,
                    "machine_id": auth.machine_id,
                })
                .to_string(),
            );
            state.last_event = orbit_types::workflow::STEERING_DELIVERED_EVENT.into();
        }
    }
    Ok(())
}

fn decide_update(
    auth: &ClaimInvocation,
    value: &ClaimWorkerUpdate,
    decision: &mut ClaimDecision,
) -> Result<(), OrbitError> {
    if auth.operator
        || !matches!(
            decision.claim.phase,
            ExecutionClaimPhase::Running | ExecutionClaimPhase::HandedOff
        )
        || value
            .expected_status
            .is_some_and(|status| status != decision.expected_status)
    {
        return Err(stale_claim());
    }
    if let Some(status) = value.status {
        decide_update_status(auth, status, value, decision)?;
    }
    check_update_scope(value, decision)?;
    decision.evidence = value.evidence.clone();
    decision.params.status_note = value.status_note.clone();
    decision.handoff_effects.worker_update = Some(value.clone());
    decision.state.last_event = "claim_updated".into();
    Ok(())
}

/// A worker's status change: only a running claim's block settles as a
/// failure; any other change is a lifecycle transition the worker cannot make.
fn decide_update_status(
    auth: &ClaimInvocation,
    status: TaskStatus,
    value: &ClaimWorkerUpdate,
    decision: &mut ClaimDecision,
) -> Result<(), OrbitError> {
    let ClaimDecision {
        claim,
        expected_status,
        state,
        params,
        release,
        ..
    } = decision;
    if status == TaskStatus::Blocked && claim.phase == ExecutionClaimPhase::Running {
        if value
            .evidence
            .summary
            .as_deref()
            .is_none_or(|text| text.trim().is_empty())
        {
            return Err(invalid("failure settlement requires evidence"));
        }
        state.claim.phase = ExecutionClaimPhase::Failed;
        state.landing_invalidated = true;
        state.settlement = Some(ClaimSettlementRecord::of(
            ClaimSettlementKind::Fail,
            &value.evidence,
            &auth.attribution,
            Utc::now().to_rfc3339(),
        ));
        params.rows.extend(leaf_run_evidence_row(
            claim,
            state,
            value.evidence.leaf_run.as_ref(),
        )?);
        params.status = Some(status);
        *release = true;
    } else if status != *expected_status {
        return Err(invalid(
            "worker lifecycle transition requires typed handoff",
        ));
    }
    Ok(())
}

/// A worker update never changes the claim's footprint, and a handed-off
/// claim accepts no new evidence, plan, note or external reference.
fn check_update_scope(
    value: &ClaimWorkerUpdate,
    decision: &ClaimDecision,
) -> Result<(), OrbitError> {
    let ClaimDecision { claim, bundle, .. } = decision;
    if value
        .context_files
        .as_ref()
        .is_some_and(|context| context != &bundle.envelope.context_files)
    {
        return Err(invalid(
            "claim footprint changes require recovery and readmission",
        ));
    }
    if claim.phase == ExecutionClaimPhase::HandedOff
        && (value.evidence != ClaimEvidence::default()
            || value.plan.is_some()
            || value.status_note.is_some()
            || value
                .external_refs
                .iter()
                .any(|reference| !bundle.envelope.external_refs.contains(reference)))
    {
        return Err(stale_claim());
    }
    Ok(())
}

fn decide_friction(
    auth: &ClaimInvocation,
    value: &FrictionAddParams,
    receipt_id: &str,
    decision: &mut ClaimDecision,
) -> Result<(), OrbitError> {
    if auth.operator
        || decision.claim.phase != ExecutionClaimPhase::Running
        || value
            .during_task
            .as_ref()
            .is_some_and(|task| task != &auth.task_id)
    {
        return Err(stale_claim());
    }
    let mut value = value.clone();
    value.during_task = Some(auth.task_id.clone());
    decision.handoff_effects.friction = Some((value, receipt_id.to_string()));
    decision.state.last_event = "claim_friction".into();
    Ok(())
}

/// The evidence an unsettled claim's worker reports, shared by evidence
/// records and failures.
fn decide_evidence(
    auth: &ClaimInvocation,
    value: &ClaimEvidence,
    decision: &mut ClaimDecision,
) -> Result<(), OrbitError> {
    if auth.operator
        || !matches!(
            decision.claim.phase,
            ExecutionClaimPhase::Claimed | ExecutionClaimPhase::Running
        )
    {
        return Err(stale_claim());
    }
    decision.evidence = value.clone();
    decision.state.last_event = "claim_evidence".into();
    Ok(())
}

/// [ORB-20906] The immutable evidence row a failure or release settlement
/// publishes with its commit, when the follower captured its leaf's run
/// record. The owner binds the report to the claim it holds: a report for any
/// run but the claim's bound one is dropped, since a follower's report is
/// evidence and never changes how the settlement applies.
pub(super) fn leaf_run_evidence_row(
    claim: &ExecutionClaim,
    state: &ClaimInspection,
    report: Option<&LeafRunReport>,
) -> Result<Option<TaskCoordinationRow>, OrbitError> {
    let Some(report) = report else {
        return Ok(None);
    };
    let Some(bound) = state.bound_run.as_ref() else {
        return Ok(None);
    };
    if bound.run_id != report.run_id
        || bound.machine_id != claim.executed_on.machine_id
        || report.job_id.trim().is_empty()
    {
        orbit_common::tracing::warn!(
            target: "orbit.store.claim_evidence",
            claim_id = %claim.claim_id,
            "a settlement's leaf run report names another run than the claim's bound leaf; it is not recorded",
        );
        return Ok(None);
    }
    let report = LeafRunReport {
        root_cause: report
            .root_cause
            .clone()
            .map(LeafRootCause::bounded)
            .filter(|cause| !cause.is_empty()),
        ..report.clone()
    };
    Ok(Some(row(
        LEAF_RUN_EVIDENCE,
        &claim.claim_id,
        &LeafRunEvidence {
            provenance: LeafEvidenceProvenance::Settlement,
            recorded_at: Utc::now().to_rfc3339(),
            claim_id: claim.claim_id.clone(),
            task_id: claim.task_id.clone(),
            machine_id: claim.executed_on.machine_id.clone(),
            drain_run_id: claim.run_context.run_id.clone(),
            report,
        },
    )?))
}
