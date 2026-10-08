use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::TaskStatus;

use super::super::TaskCommitBoundary;
use super::super::landing::LandingStop;
use super::codec::{CLAIM, RECEIPT, RELEASE_BUDGET, RELEASE_BUDGET_WINDOW_HOURS, STATE};
use super::releases::release_budget_comment;
use super::{ClaimAuthority, MutationReceipt, decode, encode, invalid, row};
use crate::contracts::*;

impl TaskCommitBoundary {
    /// Trusted-context seam for application callers and the later generic tool adapter.
    /// Missing invocation context is always refused, even for an unclaimed task.
    pub fn mutate_execution_claim(
        &self,
        invocation: Option<&ClaimInvocation>,
        mutation_id: &str,
        mutation: &ClaimMutation,
    ) -> Result<ClaimMutationResult, OrbitError> {
        let auth = invocation.ok_or_else(|| invalid("claim invocation context required"))?;
        if [mutation_id, &auth.task_id, &auth.claim_id, &auth.machine_id]
            .iter()
            .any(|s| s.trim().is_empty())
        {
            return Err(invalid("invalid claim invocation"));
        }
        self.with_admission(|| self.mutate_claim_locked(auth, mutation_id, mutation))
    }

    /// The claim `auth` acts under, refused as `stale_claim` unless it is the
    /// task's current, unsettled claim, held by the invoking machine and bound
    /// run, with the task in the status the claim's phase implies. Every claim
    /// mutation and every worker read fence stands on exactly these checks.
    /// `recovering` admits a failed claim for an operator's recovery.
    fn claim_authority(
        &self,
        auth: &ClaimInvocation,
        recovering: bool,
    ) -> Result<ClaimAuthority, OrbitError> {
        let row = self
            .coordination_row(CLAIM, &auth.claim_id)?
            .ok_or_else(|| invalid("stale_claim"))?;
        let claim: ExecutionClaim = decode(&row.payload_json)?;
        let recover_failed =
            auth.operator && recovering && claim.phase == ExecutionClaimPhase::Failed;
        if claim.task_id != auth.task_id || !(claim.phase.is_unsettled() || recover_failed) {
            return Err(invalid("stale_claim"));
        }
        let state = self.claim_state(claim.clone())?;
        if (matches!(
            claim.phase,
            ExecutionClaimPhase::Running
                | ExecutionClaimPhase::HandedOff
                | ExecutionClaimPhase::RepairPending
        ) && state.bound_run.is_none())
            || (claim.phase == ExecutionClaimPhase::Claimed && state.bound_run.is_some())
        {
            return Err(invalid("claim binding is inconsistent with phase"));
        }
        let state_row = self.coordination_row(STATE, &claim.claim_id)?;
        if !auth.operator
            && (auth.machine_id != claim.executed_on.machine_id || auth.run != state.bound_run)
        {
            return Err(invalid("stale_claim"));
        }
        let bundle = self.bundle_store.read_bundle_lightweight(&claim.task_id)?;
        let current_claim = bundle
            .events
            .iter()
            .rev()
            .find(|e| e.event_type == "pulled_by")
            .and_then(|e| e.note.as_deref())
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        if current_claim
            .as_ref()
            .and_then(|v| v.get("claim_id"))
            .and_then(serde_json::Value::as_str)
            != Some(claim.claim_id.as_str())
        {
            return Err(invalid("stale_claim"));
        }
        let expected_status = match claim.phase {
            ExecutionClaimPhase::HandedOff => TaskStatus::Review,
            ExecutionClaimPhase::Failed => TaskStatus::Blocked,
            ExecutionClaimPhase::Landed => TaskStatus::Done,
            ExecutionClaimPhase::Claimed
            | ExecutionClaimPhase::Running
            | ExecutionClaimPhase::Revoked
            | ExecutionClaimPhase::RepairPending => TaskStatus::InProgress,
        };
        if bundle.envelope.status != expected_status {
            return Err(invalid("stale_claim"));
        }
        if let Some(bound) = &state.bound_run
            && (bundle.envelope.job_run_id.as_deref() != Some(&bound.run_id)
                || bundle
                    .envelope
                    .job_run_machine
                    .as_ref()
                    .map(|location| location.machine_id.as_str())
                    != Some(bound.machine_id.as_str()))
        {
            return Err(invalid("stale_claim"));
        }
        Ok(ClaimAuthority {
            row,
            claim,
            state,
            state_row,
            bundle,
            expected_status,
        })
    }

    /// [ORB-14221] The read-side counterpart of a worker's claim mutation:
    /// succeed only while `auth` could still update its claim. A claim that
    /// was released, failed, revoked by recovery, landed or superseded by a
    /// later pull, or that is bound to another run, is refused as
    /// `stale_claim`, exactly as that worker's write would be. Reads the claim
    /// journal and the task bundle and records nothing.
    pub fn verify_worker_claim(&self, auth: &ClaimInvocation) -> Result<(), OrbitError> {
        if auth.operator
            || [&auth.task_id, &auth.claim_id, &auth.machine_id]
                .iter()
                .any(|s| s.trim().is_empty())
        {
            return Err(invalid("invalid claim invocation"));
        }
        self.with_admission(|| {
            let authority = self.claim_authority(auth, false)?;
            if !matches!(
                authority.claim.phase,
                ExecutionClaimPhase::Running | ExecutionClaimPhase::HandedOff
            ) {
                return Err(invalid("stale_claim"));
            }
            Ok(())
        })
    }

    fn mutate_claim_locked(
        &self,
        auth: &ClaimInvocation,
        mutation_id: &str,
        mutation: &ClaimMutation,
    ) -> Result<ClaimMutationResult, OrbitError> {
        let mut identity = mutation.clone();
        if let ClaimMutation::Friction(params) = &mut identity {
            params.created_at = chrono::DateTime::UNIX_EPOCH;
        }
        let input = encode(&(
            &auth.task_id,
            &auth.claim_id,
            &auth.machine_id,
            &auth.run,
            auth.operator,
            &identity,
        ))?;
        let receipt_id = sha256_hex(encode(&(&auth.claim_id, mutation_id))?.as_bytes());
        if let Some(receipt) = self.coordination_row(RECEIPT, &receipt_id)? {
            let receipt: MutationReceipt = decode(&receipt.payload_json)?;
            if receipt.input != input {
                return Err(invalid("mutation_mismatch"));
            }
            // A persisted send intent is uncertainty, never permission to send again.
            // The landing consumer must reconcile external state, including after a
            // later revocation or a changed candidate. Do not replay launch authority.
            if matches!(
                mutation,
                ClaimMutation::MergeIntent {
                    resolved: false,
                    ..
                }
            ) {
                return Err(invalid("merge intent replay requires reconciliation"));
            }
            // Binding is also a launch gate. Never replay historical launch permission
            // after handoff, failure, or revocation; other receipts are advisory outcomes.
            if let ClaimMutation::Bind { run, .. } = mutation {
                let current = self
                    .execution_claims()?
                    .into_iter()
                    .find(|c| c.claim_id == auth.claim_id)
                    .ok_or_else(|| invalid("stale_claim"))?;
                if current.phase != ExecutionClaimPhase::Running
                    || self.claim_state(current)?.bound_run.as_ref() != Some(run)
                {
                    return Err(invalid("stale_claim"));
                }
            }
            return self.with_friction_result(receipt.result, &receipt_id);
        }
        let ClaimAuthority {
            row: old,
            claim,
            mut state,
            state_row: old_state,
            bundle,
            expected_status,
        } = self.claim_authority(auth, matches!(mutation, ClaimMutation::Recover { .. }))?;
        let mut params = TaskCoordinationCommitParams {
            task_id: claim.task_id.clone(),
            actor: auth.machine_id.clone(),
            expected_status: vec![expected_status],
            ..Default::default()
        };
        let mut evidence = ClaimEvidence::default();
        let mut binding = None;
        let mut release = false;
        let mut handoff_effects = ClaimCommitEffects::default();
        match mutation {
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
                    return Err(invalid("stale_claim"));
                }
                if let Some(status) = value.status {
                    if status == TaskStatus::Blocked && claim.phase == ExecutionClaimPhase::Running
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
                        || value
                            .external_refs
                            .iter()
                            .any(|reference| !bundle.envelope.external_refs.contains(reference)))
                {
                    return Err(invalid("stale_claim"));
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
                    return Err(invalid("stale_claim"));
                }
                let mut value = value.clone();
                value.during_task = Some(auth.task_id.clone());
                handoff_effects.friction = Some((value, receipt_id.clone()));
                state.last_event = "claim_friction".into();
            }
            ClaimMutation::Bind { run, ship } => {
                if auth.operator
                    || claim.phase != ExecutionClaimPhase::Claimed
                    || state.bound_run.is_some()
                    || run.machine_id != auth.machine_id
                    || run.run_id.trim().is_empty()
                {
                    return Err(invalid("stale_claim"));
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
                if self.coordination_rows(STATE)?.iter().any(|r| {
                    decode::<ClaimInspection>(&r.payload_json)
                        .is_ok_and(|s| s.bound_run.as_ref() == Some(run))
                }) {
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
                    return Err(invalid("unresolved external merge intent"));
                }
                if self.accepted_handoff(&auth.claim_id)?.handoff_id != *handoff_id {
                    return Err(invalid("handoff identity mismatch"));
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
                    return Err(invalid("stale_claim"));
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
                    state.settlement = Some(ClaimSettlementRecord::of(
                        ClaimSettlementKind::Fail,
                        value,
                        Utc::now().to_rfc3339(),
                    ));
                    state.preserved_candidate = self.preserve_candidate(&claim.task_id, value)?;
                }
            }
            ClaimMutation::Release(value) => {
                if auth.operator
                    || !matches!(
                        claim.phase,
                        ExecutionClaimPhase::Claimed | ExecutionClaimPhase::Running
                    )
                {
                    return Err(invalid("stale_claim"));
                }
                let Some(reason) = value.summary.as_deref().filter(|s| !s.trim().is_empty()) else {
                    return Err(invalid("claim release requires a reason"));
                };
                let released_at = Utc::now();
                // [ORB-14257] A typed failure releases only when its class
                // does not block and the task's release budget allows it. A
                // provider usage limit is not counted [ORB-14695].
                let blocked = match &value.failure {
                    Some(failure) if failure.class.blocks() => Some(None),
                    Some(failure) if failure.budgeted() => {
                        let earlier = self.budgeted_releases(&claim.task_id, released_at)?;
                        (earlier.len() >= RELEASE_BUDGET)
                            .then(|| Some(release_budget_comment(&earlier, failure, &released_at)))
                    }
                    _ => None,
                };
                state.release = value.failure.as_ref().map(|failure| ClaimReleaseRecord {
                    class: failure.class,
                    reason: failure.reason.clone(),
                    released_at: released_at.to_rfc3339(),
                    budget_exhausted: matches!(blocked, Some(Some(_))),
                    forge_unavailable: value.forge_hold.is_some(),
                    provider_limit: failure.provider_limit,
                });
                state.settlement = Some(ClaimSettlementRecord::of(
                    ClaimSettlementKind::Release,
                    value,
                    released_at.to_rfc3339(),
                ));
                state.preserved_candidate = self.preserve_candidate(&claim.task_id, value)?;
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
                        reason.to_string()
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
                        params.status_note = Some(format!(
                            "run={}; candidate={}; awaiting named external checks; receipt \
                             queues a fresh review. {reason}",
                            hold.run_id, hold.candidate.commit
                        ));
                        state.last_event = "review_awaiting_evidence".into();
                    // [ORB-14258] A release for a red base is recorded as the
                    // hold itself, so owner admission withholds the task until
                    // the command passes on a new base tip.
                    } else if let Some(hold) = &value.baseline_red {
                        params.status_note = Some(hold.text(reason));
                        state.last_event = orbit_types::workflow::BASELINE_RED_HOLD_EVENT.into();
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
                    return Err(invalid("unresolved external merge intent"));
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
                    &mut params,
                    &mut handoff_effects,
                )?;
                state.claim.phase = ExecutionClaimPhase::Landed;
                state.last_event = "landing_completed".into();
            }
            ClaimMutation::StopLanding {
                handoff_id,
                reason,
                repairable,
            } => {
                match self.stop_landing_attempt(
                    auth,
                    &state,
                    handoff_id,
                    reason,
                    *repairable,
                    &mut params,
                    &mut handoff_effects,
                )? {
                    LandingStop::Stopped => {}
                    LandingStop::Repair => {
                        state.claim.phase = ExecutionClaimPhase::RepairPending;
                        state.landing_invalidated = true;
                    }
                    LandingStop::Blocked(comment) => {
                        state.claim.phase = ExecutionClaimPhase::Failed;
                        state.landing_invalidated = true;
                        evidence.comment = Some(comment);
                        release = true;
                    }
                }
                state.last_event = "landing_stopped".into();
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
        state.updated_at = Utc::now().to_rfc3339();
        state.age_seconds = None;
        params.status_event = Some(state.last_event.clone());
        let result = ClaimMutationResult {
            claim_id: claim.claim_id.clone(),
            phase: state.claim.phase,
            status: params.status.unwrap_or(expected_status),
            friction: None,
        };
        params.rows.push(row(
            RECEIPT,
            &receipt_id,
            &MutationReceipt {
                input,
                result: result.clone(),
            },
        )?);
        let new_state = row(STATE, &claim.claim_id, &state)?;
        let mut effects = ClaimCommitEffects {
            execution_origin: Some(claim.executed_on.clone()),
            worker_update: handoff_effects.worker_update,
            friction: handoff_effects.friction,
            replacements: handoff_effects.replacements,
            release_reservation: release.then_some(claim.reservation_id),
        };
        effects
            .replacements
            .push((old, row(CLAIM, &claim.claim_id, &state.claim)?));
        if let Some(old_state) = old_state {
            effects.replacements.push((old_state, new_state));
        } else {
            params.rows.push(new_state);
        }
        let outcome = self.commit_locked_effects(
            &params,
            &mut |_| Ok(params.rows.clone()),
            &effects,
            &evidence,
            binding.as_ref(),
        )?;
        match outcome {
            TaskCoordinationCommitOutcome::Committed(_) => {
                self.with_friction_result(result, &receipt_id)
            }
            _ => Err(invalid("stale_claim")),
        }
    }
}
