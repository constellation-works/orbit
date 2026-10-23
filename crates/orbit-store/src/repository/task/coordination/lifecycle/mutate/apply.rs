use chrono::Utc;
use orbit_common::fs::io::hold_step;
use orbit_common::{ClaimRefusalKind, OrbitError};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{ClaimFailureClass, ReviewGateHistoryNote};

use super::super::super::TaskCommitBoundary;
use super::super::super::landing::LandingStop;
use super::super::codec::{
    CLAIM, HANDOFF_SETTLEMENT, OWNER_BUSY_RELEASE_ALLOWANCE, RECEIPT, RELEASE_BUDGET,
    RELEASE_BUDGET_WINDOW_HOURS, STATE,
};
use super::super::releases::release_budget_comment;
use super::super::{ClaimAuthority, MutationReceipt, decode_state, encode, invalid, row};
use crate::contracts::*;

/// Steps of a claim mutation's admission section, so a long hold's report
/// names where it went rather than the section's whole operation.
const RECEIPT_STEP: &str = "claim receipt";
const AUTHORITY_STEP: &str = "claim authority";
const DECISION_STEP: &str = "claim decision";
const COMMIT_STEP: &str = "claim commit";

impl TaskCommitBoundary {
    pub(super) fn mutate_claim_locked(
        &self,
        auth: &ClaimInvocation,
        mutation: &ClaimMutation,
        receipt_id: &str,
        input: String,
    ) -> Result<ClaimMutationResult, OrbitError> {
        if let Some(result) = hold_step(RECEIPT_STEP, || {
            self.replay_claim_receipt(auth, mutation, receipt_id, &input)
        })? {
            return Ok(result);
        }
        let ClaimAuthority {
            row: old,
            claim,
            mut state,
            state_row: old_state,
            bundle,
            expected_status,
        } = hold_step(AUTHORITY_STEP, || {
            self.claim_authority(auth, matches!(mutation, ClaimMutation::Recover { .. }))
        })?;
        let mut params = TaskCoordinationCommitParams {
            task_id: claim.task_id.clone(),
            actor: auth.machine_id.clone(),
            expected_status: vec![expected_status],
            ..Default::default()
        };
        let mut evidence = ClaimEvidence::default();
        let mut binding = None;
        let mut release = false;
        // A steering snapshot changes only the claim's own record, so it
        // writes no task history event.
        let mut history_event = true;
        let mut handoff_effects = ClaimCommitEffects::default();
        hold_step(DECISION_STEP, || -> Result<(), OrbitError> {
            match mutation {
                ClaimMutation::Steering(record) => {
                    if auth.operator || claim.phase != ExecutionClaimPhase::Running {
                        return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
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
                            history_event = false;
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
                            state.last_event =
                                orbit_types::workflow::STEERING_DELIVERED_EVENT.into();
                        }
                    }
                }
                ClaimMutation::Update(value) => {
                    if auth.operator
                        || !matches!(
                            claim.phase,
                            ExecutionClaimPhase::Running | ExecutionClaimPhase::HandedOff
                        )
                        || value
                            .expected_status
                            .is_some_and(|status| status != expected_status)
                    {
                        return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
                    }
                    if let Some(status) = value.status {
                        if status == TaskStatus::Blocked
                            && claim.phase == ExecutionClaimPhase::Running
                        {
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
                            params.status = Some(status);
                            release = true;
                        } else if status != expected_status {
                            return Err(invalid(
                                "worker lifecycle transition requires typed handoff",
                            ));
                        }
                    }
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
                            || value.external_refs.iter().any(|reference| {
                                !bundle.envelope.external_refs.contains(reference)
                            }))
                    {
                        return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
                    }
                    evidence = value.evidence.clone();
                    params.status_note = value.status_note.clone();
                    handoff_effects.worker_update = Some(value.clone());
                    state.last_event = "claim_updated".into();
                }
                ClaimMutation::Friction(value) => {
                    if auth.operator
                        || claim.phase != ExecutionClaimPhase::Running
                        || value
                            .during_task
                            .as_ref()
                            .is_some_and(|task| task != &auth.task_id)
                    {
                        return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
                    }
                    let mut value = value.clone();
                    value.during_task = Some(auth.task_id.clone());
                    handoff_effects.friction = Some((value, receipt_id.to_string()));
                    state.last_event = "claim_friction".into();
                }
                ClaimMutation::Bind { run, ship } => {
                    if auth.operator
                        || claim.phase != ExecutionClaimPhase::Claimed
                        || state.bound_run.is_some()
                        || run.machine_id != auth.machine_id
                        || run.run_id.trim().is_empty()
                    {
                        return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
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
                        .coordination_rows_mentioning(STATE, &run.run_id)?
                        .iter()
                        .any(|r| {
                            decode_state(&r.payload_json)
                                .is_ok_and(|s| s.bound_run.as_ref() == Some(run))
                        })
                    {
                        return Err(invalid("leaf run already bound"));
                    }
                    binding = Some(run.clone());
                    state.bound_run = binding.clone();
                    state.claim.phase = ExecutionClaimPhase::Running;
                    state.last_event = "claim_bound".into();
                }
                ClaimMutation::Handoff(_) => return Err(invalid("typed handoff required")),
                ClaimMutation::AcceptHandoff(handoff) => {
                    state.claim.footprint = self.accept_typed_handoff(
                        auth,
                        &state,
                        handoff,
                        &mut params,
                        &mut handoff_effects,
                    )?;
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
                            &evidence,
                            &auth.attribution,
                            Utc::now().to_rfc3339(),
                        ),
                    )?);
                    state.claim.phase = ExecutionClaimPhase::HandedOff;
                    params.status = Some(TaskStatus::Review);
                    release = true;
                    state.last_event = "claim_handed_off".into();
                }
                ClaimMutation::ApproveHandoff {
                    handoff_id,
                    candidate,
                } => {
                    self.approve_typed_handoff(auth, &state, handoff_id, candidate, &mut params)?;
                    state.last_event = "handoff_approved".into();
                }
                ClaimMutation::RevokeHandoff { handoff_id, reason } => {
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
                    if self.accepted_handoff(&auth.claim_id)?.handoff_id != *handoff_id {
                        return Err(OrbitError::claim_refused(
                            ClaimRefusalKind::HandoffIdentityMismatch,
                        ));
                    }
                    self.revoke_handoff_authority(auth, reason, &mut params, &mut handoff_effects)?;
                    state.landing_invalidated = true;
                    state.last_event = "handoff_revoked".into();
                    params.status_note = Some(reason.clone());
                }
                ClaimMutation::Evidence(value) | ClaimMutation::Fail(value) => {
                    if auth.operator
                        || !matches!(
                            claim.phase,
                            ExecutionClaimPhase::Claimed | ExecutionClaimPhase::Running
                        )
                    {
                        return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
                    }
                    evidence = value.clone();
                    state.last_event = "claim_evidence".into();
                    if matches!(mutation, ClaimMutation::Fail(_)) {
                        if value.summary.as_deref().is_none_or(|s| s.trim().is_empty()) {
                            return Err(invalid("failure settlement requires evidence"));
                        }
                        state.claim.phase = ExecutionClaimPhase::Failed;
                        state.landing_invalidated = true;
                        params.status = Some(TaskStatus::Blocked);
                        release = true;
                        state.last_event = "claim_failed".into();
                        let settlement = ClaimSettlementRecord::of(
                            ClaimSettlementKind::Fail,
                            value,
                            &auth.attribution,
                            Utc::now().to_rfc3339(),
                        );
                        params.status_note = Some(
                            ClaimFailedNote::of(
                                &settlement,
                                &claim.executed_on,
                                state.bound_run.as_ref(),
                            )
                            .text(),
                        );
                        state.settlement = Some(settlement);
                        state.preserved_candidate =
                            self.preserve_candidate(&claim.task_id, value)?;
                        params.append_history.extend(Self::kept_candidate_entry(
                            &claim,
                            state.preserved_candidate.as_ref(),
                        ));
                    }
                }
                ClaimMutation::Release(value) => {
                    if auth.operator
                        || !matches!(
                            claim.phase,
                            ExecutionClaimPhase::Claimed | ExecutionClaimPhase::Running
                        )
                    {
                        return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
                    }
                    let Some(reason) = value.summary.as_deref().filter(|s| !s.trim().is_empty())
                    else {
                        return Err(invalid("claim release requires a reason"));
                    };
                    let released_at = Utc::now();
                    // A busy owner exempts a release only for a `transient`
                    // failure, and only for the task's first few such releases
                    // in the budget window; any other is an ordinary release.
                    let failure = match &value.failure {
                        Some(failure) if failure.owner_busy => {
                            let exempt = failure.class == ClaimFailureClass::Transient
                                && self.owner_busy_releases(&claim.task_id, released_at)?.len()
                                    < OWNER_BUSY_RELEASE_ALLOWANCE;
                            Some(ClaimFailure {
                                owner_busy: exempt,
                                ..failure.clone()
                            })
                        }
                        failure => failure.clone(),
                    };
                    // [ORB-14257] A typed failure releases only when its class
                    // does not block and the task's release budget allows it. A
                    // provider usage limit is not counted [ORB-14695].
                    let blocked = match &failure {
                        Some(failure) if failure.class.blocks() => Some(None),
                        Some(failure) if failure.budgeted() => {
                            let earlier = self.budgeted_releases(&claim.task_id, released_at)?;
                            (earlier.len() >= RELEASE_BUDGET).then(|| {
                                Some(release_budget_comment(&earlier, failure, &released_at))
                            })
                        }
                        _ => None,
                    };
                    state.release = failure.as_ref().map(|failure| ClaimReleaseRecord {
                        class: failure.class,
                        reason: failure.reason.clone(),
                        released_at: released_at.to_rfc3339(),
                        budget_exhausted: matches!(blocked, Some(Some(_))),
                        forge_unavailable: value.forge_hold.is_some(),
                        provider_limit: failure.provider_limit,
                        owner_busy: failure.owner_busy,
                    });
                    let settlement = ClaimSettlementRecord::of(
                        ClaimSettlementKind::Release,
                        value,
                        &auth.attribution,
                        released_at.to_rfc3339(),
                    );
                    let failed_note = ClaimFailedNote::of(
                        &settlement,
                        &claim.executed_on,
                        state.bound_run.as_ref(),
                    )
                    .text();
                    state.settlement = Some(settlement);
                    state.preserved_candidate = self.preserve_candidate(&claim.task_id, value)?;
                    params.append_history.extend(Self::kept_candidate_entry(
                        &claim,
                        state.preserved_candidate.as_ref(),
                    ));
                    state.landing_invalidated = true;
                    release = true;
                    if let Some(budget_comment) = blocked {
                        let exhausted = budget_comment.is_some();
                        evidence = ClaimEvidence {
                            comment: budget_comment.or_else(|| value.comment.clone()),
                            ..ClaimEvidence::default()
                        };
                        state.claim.phase = ExecutionClaimPhase::Failed;
                        params.status = Some(TaskStatus::Blocked);
                        params.status_note = Some(if exhausted {
                            format!(
                                "release budget exhausted ({RELEASE_BUDGET} in \
                                 {RELEASE_BUDGET_WINDOW_HOURS}h): {reason}"
                            )
                        } else {
                            failed_note
                        });
                        state.last_event = if exhausted {
                            "claim_release_budget_exhausted"
                        } else {
                            "claim_failed"
                        }
                        .into();
                    } else {
                        // The task goes back to the backlog untouched: its
                        // execution summary stays whatever the last real attempt
                        // left, and the reason travels as the status note and the
                        // comment.
                        evidence = ClaimEvidence {
                            comment: value.comment.clone(),
                            ..ClaimEvidence::default()
                        };
                        state.claim.phase = ExecutionClaimPhase::Revoked;
                        params.status = Some(TaskStatus::Backlog);
                        // A release for a review evidence hold keeps the task in
                        // progress under the hold, the latest decision, so
                        // receipt of the named evidence queues a fresh review.
                        // The note names the held run as a local hold's does.
                        if let Some(hold) = &value.evidence_hold {
                            params.status = None;
                            params.status_note =
                                Some(ReviewGateHistoryNote::awaiting_evidence_text(
                                    &hold.run_id,
                                    &hold.candidate.commit,
                                    Some(reason),
                                ));
                            state.last_event = "review_awaiting_evidence".into();
                        // [ORB-14258] A release for a red base is recorded as the
                        // hold itself, so owner admission withholds the task until
                        // the command passes on a new base tip.
                        } else if let Some(hold) = &value.baseline_red {
                            params.status_note = Some(hold.text(reason));
                            state.last_event =
                                orbit_types::workflow::BASELINE_RED_HOLD_EVENT.into();
                        // [ORB-15573] A release its leaf's provider refused is
                        // recorded as the hold the owner computed, so owner and
                        // pull admission draw the task onto another provider.
                        } else if let Some(hold) = &auth.provider_hold {
                            params.status_note = Some(hold.text(reason));
                            state.last_event =
                                orbit_types::workflow::PROVIDER_FAILURE_HOLD_EVENT.into();
                        } else {
                            params.status_note = Some(reason.to_string());
                            state.last_event = "claim_released".into();
                        }
                    }
                }
                ClaimMutation::Recover { status, reason } => {
                    if !auth.operator {
                        return Err(invalid("operator recovery capability required"));
                    }
                    if state.unresolved_merge_intent.is_some() {
                        return Err(OrbitError::claim_refused(
                            ClaimRefusalKind::UnresolvedMergeIntent,
                        ));
                    }
                    if !matches!(status, TaskStatus::Blocked | TaskStatus::Backlog)
                        || reason.trim().is_empty()
                    {
                        return Err(invalid(
                            "recovery requires a reason and blocked or backlog target",
                        ));
                    }
                    self.revoke_handoff_authority(auth, reason, &mut params, &mut handoff_effects)?;
                    state.claim.phase = ExecutionClaimPhase::Revoked;
                    state.landing_invalidated = true;
                    params.status = Some(*status);
                    params.status_note = Some(reason.clone());
                    release = true;
                    state.last_event = "claim_revoked".into();
                }
                ClaimMutation::AttachLandingRetry {
                    handoff_id,
                    request_key,
                    job_run_id,
                } => {
                    self.attach_landing_retry(
                        auth,
                        &state,
                        handoff_id,
                        request_key,
                        job_run_id,
                        &mut params,
                        &mut handoff_effects,
                    )?;
                    state.last_event = "landing_dispatched".into();
                }
                ClaimMutation::ReserveBaseUpdate(request) => {
                    use orbit_types::workflow::{
                        PR_BASE_UPDATE_RESERVED_EVENT, PrBaseUpdateAdmission, decide_pr_base_update,
                    };
                    self.check_base_update_scope(auth, &state, request)?;
                    let history = crate::repository::task::v2::task_history_from_events(
                        bundle.events.clone(),
                    );
                    if !matches!(
                        decide_pr_base_update(&history, request),
                        PrBaseUpdateAdmission::Reserved
                    ) {
                        return Err(invalid("the cumulative base update is already reserved"));
                    }
                    params
                        .append_history
                        .push(orbit_types::task::TaskHistoryEntry {
                            at: Utc::now(),
                            by: auth.machine_id.clone(),
                            event: PR_BASE_UPDATE_RESERVED_EVENT.into(),
                            note: Some(encode(request)?),
                            from_status: None,
                            to_status: None,
                            operation_id: None,
                            decision: None,
                        });
                    state.last_event = "handoff_base_update_admitted".into();
                }
                ClaimMutation::RetryLanding(request) => {
                    self.reserve_landing_retry(
                        auth,
                        &state,
                        request,
                        &mut params,
                        &mut handoff_effects,
                    )?;
                    state.last_event = "landing_retry_reserved".into();
                }
                ClaimMutation::DispatchLanding {
                    handoff_id,
                    job_run_id,
                } => {
                    self.dispatch_landing_attempt(
                        auth,
                        &state,
                        handoff_id,
                        job_run_id.as_ref(),
                        &mut params,
                        &mut handoff_effects,
                    )?;
                    state.last_event = "landing_dispatched".into();
                }
                ClaimMutation::CompleteLanding {
                    handoff_id,
                    evidence: proof,
                } => {
                    self.complete_landing_attempt(
                        auth,
                        &state,
                        handoff_id,
                        proof,
                        &crate::repository::task::v2::task_history_from_events(
                            bundle.events.clone(),
                        ),
                        &mut params,
                        &mut handoff_effects,
                    )?;
                    state.claim.phase = ExecutionClaimPhase::Landed;
                    state.last_event = "landing_completed".into();
                }
                ClaimMutation::SettleExternalMerge {
                    handoff_id,
                    pull_request,
                    base_branch,
                    merge_commit,
                    note,
                } => {
                    if note.trim().is_empty() {
                        return Err(invalid("external merge settlement requires a landing note"));
                    }
                    self.check_external_merge_settlement(
                        auth,
                        &state,
                        handoff_id,
                        *pull_request,
                        base_branch,
                        merge_commit,
                        &crate::repository::task::v2::task_history_from_events(
                            bundle.events.clone(),
                        ),
                    )?;
                    // `Revoked`, not `Landed`: `Landed` says the owner's landing
                    // consumer verified the merge under a current completion
                    // authorization, and this handoff's authorization was
                    // withdrawn. The claim ends as revoked work does; the task's
                    // history and its landing note record the external merge.
                    state.claim.phase = ExecutionClaimPhase::Revoked;
                    state.landing_invalidated = true;
                    release = true;
                    params.status = Some(TaskStatus::Done);
                    params.status_note = Some(note.clone());
                    handoff_effects.worker_update = Some(ClaimWorkerUpdate {
                        external_refs: vec![orbit_types::task::ExternalRef::github_pr(
                            pull_request.to_string(),
                        )?],
                        ..ClaimWorkerUpdate::default()
                    });
                    state.last_event =
                        orbit_types::workflow::handoff::EXTERNAL_MERGE_SETTLED_EVENT.into();
                }
                ClaimMutation::StopLanding {
                    handoff_id,
                    reason,
                    repairable,
                    requeue,
                } => {
                    state.last_event = "landing_stopped".into();
                    let kept = match self.stop_landing_attempt(
                        auth,
                        &state,
                        handoff_id,
                        reason,
                        *repairable,
                        *requeue,
                        &mut params,
                        &mut handoff_effects,
                    )? {
                        LandingStop::Stopped => None,
                        LandingStop::Repair => {
                            state.claim.phase = ExecutionClaimPhase::RepairPending;
                            state.landing_invalidated = true;
                            None
                        }
                        LandingStop::Blocked(comment) => {
                            state.claim.phase = ExecutionClaimPhase::Failed;
                            state.landing_invalidated = true;
                            evidence.comment = Some(comment);
                            release = true;
                            None
                        }
                        // [ORB-16890] The claim ends like a release: the task's
                        // next claim or owner run is offered the kept candidate.
                        LandingStop::Requeue(kept) => {
                            state.claim.phase = ExecutionClaimPhase::Revoked;
                            state.landing_invalidated = true;
                            release = true;
                            state.last_event =
                                orbit_types::workflow::handoff::LANDING_REPAIR_REQUEUED_EVENT
                                    .into();
                            Some(*kept)
                        }
                        LandingStop::RepairBudgetExhausted { comment, kept } => {
                            state.claim.phase = ExecutionClaimPhase::Failed;
                            state.landing_invalidated = true;
                            evidence.comment = Some(comment);
                            release = true;
                            state.last_event =
                                orbit_types::workflow::handoff::LANDING_REPAIR_EXHAUSTED_EVENT
                                    .into();
                            Some(*kept)
                        }
                    };
                    if let Some(kept) = kept {
                        state.preserved_candidate = Some(kept);
                        params.append_history.extend(Self::kept_candidate_entry(
                            &claim,
                            state.preserved_candidate.as_ref(),
                        ));
                    }
                }
                ClaimMutation::MergeIntent {
                    intent_id,
                    resolved,
                    evidence: proof,
                } => {
                    if !auth.operator
                        || claim.phase != ExecutionClaimPhase::HandedOff
                        || state.landing_invalidated
                    {
                        return Err(invalid("current operator landing context required"));
                    }
                    if intent_id.trim().is_empty() || proof.trim().is_empty() {
                        return Err(invalid("merge intent evidence required"));
                    }
                    if *resolved {
                        if state.unresolved_merge_intent.as_ref() != Some(intent_id) {
                            return Err(invalid("merge intent mismatch"));
                        }
                        state.unresolved_merge_intent = None;
                    } else {
                        if state.unresolved_merge_intent.is_some() {
                            return Err(invalid("merge intent already unresolved"));
                        }
                        self.check_handoff_landing(auth, &state)?;
                        state.unresolved_merge_intent = Some(intent_id.clone());
                    }
                    params.status_note = Some(encode(&(intent_id, resolved, proof))?);
                    state.last_event = "claim_merge_intent".into();
                }
            }
            Ok(())
        })?;
        state.updated_at = Utc::now().to_rfc3339();
        state.age_seconds = None;
        params.status_event = history_event.then(|| state.last_event.clone());
        let result = ClaimMutationResult {
            claim_id: claim.claim_id.clone(),
            phase: state.claim.phase,
            status: params.status.unwrap_or(expected_status),
            friction: None,
        };
        params.rows.push(row(
            RECEIPT,
            receipt_id,
            &MutationReceipt {
                input,
                result: result.clone(),
            },
        )?);
        let new_state = row(STATE, &claim.claim_id, &state)?;
        let mut effects = ClaimCommitEffects {
            execution_origin: Some(claim.executed_on.clone()),
            worker_update: handoff_effects.worker_update,
            regression_from: handoff_effects.regression_from,
            friction: handoff_effects.friction,
            replacements: handoff_effects.replacements,
            release_reservation: release.then_some(claim.reservation_id),
            // `claim_authority` matched a worker's machine and run to this
            // binding; an operator's mutation carries no worker evidence.
            claimed_submission: state
                .bound_run
                .clone()
                .filter(|_| !auth.operator)
                .map(|run| (claim.claim_id.clone(), run)),
            census: None,
        };
        effects
            .replacements
            .push((old, row(CLAIM, &claim.claim_id, &state.claim)?));
        if let Some(old_state) = old_state {
            effects.replacements.push((old_state, new_state));
        } else {
            params.rows.push(new_state);
        }
        // A phase that still protects the task (including repair admission,
        // which revokes and re-claims in its own commit) must not receive
        // observations queued by the sweep. Once this mutation leaves the
        // task unprotected, land them in the same commit.
        if !state.claim.phase.protects_footprint() {
            self.attach_deferred_branch_observations(&claim.task_id, &mut evidence, &mut effects)?;
        }
        let outcome = hold_step(COMMIT_STEP, || {
            self.commit_locked_effects(
                &params,
                &mut |_| Ok(params.rows.clone()),
                &effects,
                &evidence,
                binding.as_ref(),
            )
        })?;
        match outcome {
            TaskCoordinationCommitOutcome::Committed(_) => {
                self.with_friction_result(result, receipt_id)
            }
            _ => Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim)),
        }
    }
}
