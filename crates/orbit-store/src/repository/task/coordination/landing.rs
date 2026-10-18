//! The owner's durable landing attempt for an authorized handoff.
//!
//! The outbox in [`super::handoff`] records *that* a handoff may land. This
//! module records the single live attempt to land it: one attempt per handoff
//! at a time, reserved before any owner job is created, settled only against
//! verified merge evidence, and never advanced while an external merge intent
//! is unresolved.
//!
//! Nothing here performs or observes an external merge. Completion re-runs the
//! same authority and evidence rechecks the merge-intent write runs, inside the
//! claim journal transaction, so a revoked, stale or re-validated candidate
//! cannot reach `review -> done` through this path either.
use chrono::Utc;
use orbit_common::{ClaimRefusalKind, OrbitError};
use orbit_types::task::{TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::PostDeployRecord;
use orbit_types::workflow::handoff::*;
use serde_json::Value;

use super::TaskCommitBoundary;
use super::lifecycle::{decode, invalid, row};
use crate::contracts::*;
use crate::driver::sqlite::job_run_store::SqliteJobRunStore;

const ATTEMPT: &str = "distributed-landing-attempt-v1";
const HISTORY: &str = "distributed-landing-attempt-history-v1";
const RETRY: &str = "distributed-landing-retry-v1";

impl TaskCommitBoundary {
    /// The immutable operator receipt for this handoff and request key.
    pub fn landing_retry_receipt(
        &self,
        handoff_id: &str,
        request_key: &str,
    ) -> Result<Option<LandingRetryReceipt>, OrbitError> {
        self.enter_ordinary(|| {
            self.coordination_row(RETRY, &retry_id(handoff_id, request_key))?
                .map(|r| decode(&r.payload_json))
                .transpose()
        })
    }

    /// A specific attempt, including its retained stopped evidence after retry.
    pub fn landing_attempt_record(
        &self,
        handoff_id: &str,
        number: u32,
    ) -> Result<Option<LandingAttempt>, OrbitError> {
        self.enter_ordinary(|| {
            if let Some(raw) = self.attempt_row(handoff_id)? {
                let attempt: LandingAttempt = decode(&raw.payload_json)?;
                if attempt.attempt == number {
                    return Ok(Some(attempt));
                }
            }
            self.coordination_row(HISTORY, &format!("{handoff_id}:{number}"))?
                .map(|r| decode(&r.payload_json))
                .transpose()
        })
    }

    pub(super) fn verify_reserved_landing_retry(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
        request: &LandingRetryRequest,
    ) -> Result<(), OrbitError> {
        if state.claim.phase != ExecutionClaimPhase::HandedOff {
            return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
        }
        self.check_handoff_landing(auth, state)?;
        let receipt: LandingRetryReceipt = decode(
            &self
                .coordination_row(RETRY, &retry_id(&request.handoff_id, &request.request_key))?
                .ok_or_else(|| invalid("retry receipt missing"))?
                .payload_json,
        )?;
        let attempt: LandingAttempt = decode(
            &self
                .attempt_row(&request.handoff_id)?
                .ok_or_else(|| invalid("retry attempt missing"))?
                .payload_json,
        )?;
        if attempt.attempt != receipt.attempt || attempt.state != LandingAttemptState::Dispatched {
            return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn attach_landing_retry(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
        handoff_id: &str,
        request_key: &str,
        run_id: &str,
        params: &mut TaskCoordinationCommitParams,
        effects: &mut ClaimCommitEffects,
    ) -> Result<(), OrbitError> {
        let receipt: LandingRetryReceipt = decode(
            &self
                .coordination_row(RETRY, &retry_id(handoff_id, request_key))?
                .ok_or_else(|| invalid("retry receipt missing"))?
                .payload_json,
        )?;
        let attempt: LandingAttempt = decode(
            &self
                .attempt_row(handoff_id)?
                .ok_or_else(|| invalid("retry attempt missing"))?
                .payload_json,
        )?;
        if attempt.attempt != receipt.attempt {
            return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
        }
        self.dispatch_landing_attempt(
            auth,
            state,
            handoff_id,
            Some(&run_id.to_string()),
            params,
            effects,
        )
    }

    pub(super) fn reserve_landing_retry(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
        request: &LandingRetryRequest,
        params: &mut TaskCoordinationCommitParams,
        effects: &mut ClaimCommitEffects,
    ) -> Result<(), OrbitError> {
        if !auth.operator || state.claim.phase != ExecutionClaimPhase::HandedOff {
            return Err(invalid("current operator handed-off context required"));
        }
        if request.request_key.trim().is_empty()
            || request.request_key.len() > 128
            || request.reason.trim().is_empty()
            || request.reason.len() > 2000
        {
            return Err(invalid("retry requires a bounded request key and reason"));
        }
        self.check_handoff_landing(auth, state)?;
        let accepted = self.accepted_handoff(&auth.claim_id)?;
        if accepted.handoff_id != request.handoff_id
            || accepted.handoff.candidate.candidate.commit != request.expected_candidate_commit
            || accepted.handoff.candidate.base.commit != request.expected_base_commit
        {
            return Err(OrbitError::claim_refused(
                ClaimRefusalKind::HandoffCandidateMismatch,
            ));
        }
        let raw = self
            .attempt_row(&request.handoff_id)?
            .ok_or_else(|| invalid("retry requires a prior attempt"))?;
        let attempt: LandingAttempt = decode(&raw.payload_json)?;
        if attempt.attempt != request.expected_latest_attempt
            || attempt.state != request.expected_latest_state
        {
            return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
        }
        match attempt.state {
            LandingAttemptState::Stopped => {}
            LandingAttemptState::Dispatched if attempt.job_run_id.is_some() => {
                // Eligibility comes from this owner's durable run, not request
                // state, missing liveness, reservation age, or a caller boolean.
                self.terminal_landing_job_evidence(&attempt)?;
            }
            LandingAttemptState::Dispatched if state.unresolved_merge_intent.is_some() => {}
            _ => return Err(invalid("retry requires a stopped or uncertain landing")),
        }
        self.dispatch_landing_attempt(auth, state, &request.handoff_id, None, params, effects)?;
        let number =
            if attempt.state == LandingAttemptState::Dispatched && attempt.job_run_id.is_none() {
                attempt.attempt
            } else {
                attempt
                    .attempt
                    .checked_add(1)
                    .ok_or_else(|| invalid("landing attempt exhausted"))?
            };
        params.rows.push(row(
            RETRY,
            &retry_id(&request.handoff_id, &request.request_key),
            &LandingRetryReceipt {
                request: request.clone(),
                actor: auth.machine_id.clone(),
                attempt: number,
                created_at: Utc::now(),
            },
        )?);
        params.status_note = Some(request.reason.clone());
        params.status_event = Some("landing_retry_reserved".into());
        Ok(())
    }
    /// Prove the attached run belongs to this exact owner dispatch and has
    /// irreversibly ended. Read in the reservation's admission section; terminal
    /// job states cannot be restarted in place. The scoped action key fences
    /// foreign/misattached runs even when their IDs or input resemble this one.
    fn terminal_landing_job_evidence(
        &self,
        attempt: &LandingAttempt,
    ) -> Result<String, OrbitError> {
        let jobs = SqliteJobRunStore::new(self.store.clone(), &self.workspace_id);
        let run_id = attempt
            .job_run_id
            .as_deref()
            .ok_or_else(|| invalid("terminal owner landing job evidence required"))?;
        if jobs
            .automation_job_for_key(&format!(
                "landing:{}:{}",
                attempt.handoff_id, attempt.attempt
            ))?
            .as_deref()
            != Some(run_id)
        {
            return Err(invalid("landing job does not match the owner dispatch key"));
        }
        let run = jobs
            .get_job_run(run_id)?
            .ok_or_else(|| invalid("terminal owner landing job evidence missing"))?;
        if run.job_id != "task_landing_pipeline"
            || run
                .input
                .as_ref()
                .and_then(|input| input.get("handoff_id"))
                .and_then(serde_json::Value::as_str)
                != Some(attempt.handoff_id.as_str())
            || run
                .input
                .as_ref()
                .and_then(|input| input.get("task_id"))
                .and_then(serde_json::Value::as_str)
                != Some(attempt.task_id.as_str())
            || !run.state.is_terminal()
        {
            return Err(invalid("exact terminal owner landing job required"));
        }
        Ok(format!(
            "Owner landing job '{run_id}' is terminal ({}); retained for guarded recovery",
            run.state
        ))
    }

    /// The latest attempt for each handoff. Historical stops remain available
    /// through `landing_attempt_record`. Read-only owner inspection.
    pub fn landing_attempts(&self) -> Result<Vec<LandingAttempt>, OrbitError> {
        self.enter_ordinary(|| {
            self.coordination_rows(ATTEMPT)?
                .iter()
                .map(|r| decode(&r.payload_json))
                .collect()
        })
    }

    pub fn landing_attempt(&self, handoff_id: &str) -> Result<Option<LandingAttempt>, OrbitError> {
        Ok(self
            .landing_attempts()?
            .into_iter()
            .find(|attempt| attempt.handoff_id == handoff_id))
    }

    fn attempt_row(&self, handoff_id: &str) -> Result<Option<TaskCoordinationRow>, OrbitError> {
        self.coordination_row(ATTEMPT, handoff_id)
    }

    /// The live attempt for this claim's accepted handoff, with the row it was
    /// decoded from so a settlement can replace exactly that row.
    fn live_attempt(
        &self,
        auth: &ClaimInvocation,
        handoff_id: &str,
    ) -> Result<(TaskCoordinationRow, LandingAttempt), OrbitError> {
        let accepted = self.accepted_handoff(&auth.claim_id)?;
        if accepted.handoff_id != handoff_id {
            return Err(OrbitError::claim_refused(
                ClaimRefusalKind::HandoffIdentityMismatch,
            ));
        }
        let raw = self
            .attempt_row(handoff_id)?
            .ok_or_else(|| invalid("no landing attempt is dispatched for this handoff"))?;
        let attempt: LandingAttempt = decode(&raw.payload_json)?;
        if attempt.state != LandingAttemptState::Dispatched {
            return Err(invalid("landing attempt is already settled"));
        }
        Ok((raw, attempt))
    }

    /// Open the one live attempt for `handoff_id`, or attach the owner job that
    /// carries an already open attempt.
    ///
    /// `job_run_id` is the whole distinction. `None` opens an attempt: the
    /// first one, or the next after a stop or a proven terminal owner job. `Some`
    /// attaches that job to the open attempt and refuses to replace a different
    /// job — a second consumer has to open its own attempt, and the merge-intent
    /// guard still refuses a second external merge either way.
    ///
    /// Opening requires the same current landing authority the merge-intent
    /// write requires, so a revoked or invalidated handoff never reaches a job.
    pub(super) fn dispatch_landing_attempt(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
        handoff_id: &str,
        job_run_id: Option<&String>,
        params: &mut TaskCoordinationCommitParams,
        effects: &mut ClaimCommitEffects,
    ) -> Result<(), OrbitError> {
        if !auth.operator
            || state.claim.phase != ExecutionClaimPhase::HandedOff
            || state.landing_invalidated
        {
            return Err(invalid("current operator landing context required"));
        }
        if job_run_id.is_some_and(|run_id| run_id.trim().is_empty()) {
            return Err(invalid("landing job run identity required"));
        }
        let accepted = self.accepted_handoff(&auth.claim_id)?;
        if accepted.handoff_id != handoff_id {
            return Err(OrbitError::claim_refused(
                ClaimRefusalKind::HandoffIdentityMismatch,
            ));
        }
        let authorization = self.current_landing_authorization(&accepted)?;
        let now = Utc::now();
        match self.attempt_row(handoff_id)? {
            None => {
                let attempt = LandingAttempt {
                    handoff_id: handoff_id.into(),
                    authorization_id: authorization.authorization_id,
                    task_id: auth.task_id.clone(),
                    claim_id: auth.claim_id.clone(),
                    attempt: 1,
                    state: LandingAttemptState::Dispatched,
                    job_run_id: job_run_id.cloned(),
                    evidence: None,
                    created_at: now,
                    updated_at: now,
                };
                params.rows.push(row(ATTEMPT, handoff_id, &attempt)?);
            }
            Some(old) => {
                let mut attempt: LandingAttempt = decode(&old.payload_json)?;
                if attempt.state == LandingAttemptState::Merged {
                    return Err(OrbitError::claim_refused(
                        ClaimRefusalKind::HandoffAlreadyLanded,
                    ));
                }
                match job_run_id {
                    // A dispatched attempt may advance only with exact terminal
                    // owner-job evidence. Archive the reconciled stop atomically
                    // with its successor; never clear an uncertain merge intent.
                    None => {
                        if attempt.state == LandingAttemptState::Dispatched
                            && attempt.job_run_id.is_some()
                        {
                            let evidence = self.terminal_landing_job_evidence(&attempt)?;
                            attempt.state = LandingAttemptState::Stopped;
                            attempt.evidence = Some(evidence);
                            attempt.updated_at = now;
                        }
                        if attempt.state != LandingAttemptState::Dispatched
                            || attempt.job_run_id.is_some()
                        {
                            params.rows.push(row(
                                HISTORY,
                                &format!("{handoff_id}:{}", attempt.attempt),
                                &attempt,
                            )?);
                            attempt.attempt = attempt
                                .attempt
                                .checked_add(1)
                                .ok_or_else(|| invalid("landing attempt exhausted"))?;
                            attempt.state = LandingAttemptState::Dispatched;
                            attempt.job_run_id = None;
                            attempt.evidence = None;
                            attempt.created_at = now;
                        }
                    }
                    Some(run_id) => {
                        if attempt.state != LandingAttemptState::Dispatched {
                            return Err(invalid("landing attempt is not open"));
                        }
                        if attempt
                            .job_run_id
                            .as_ref()
                            .is_some_and(|recorded| recorded != run_id)
                        {
                            return Err(invalid("a landing attempt is already in flight"));
                        }
                        attempt.job_run_id = Some(run_id.clone());
                    }
                }
                attempt.authorization_id = authorization.authorization_id;
                attempt.updated_at = now;
                effects
                    .replacements
                    .push((old, row(ATTEMPT, handoff_id, &attempt)?));
            }
        }
        params.status_event = Some("landing_dispatched".into());
        Ok(())
    }

    /// Complete an authorized landing. `evidence` is the owner's verified merge
    /// evidence — the provider's merged pull request or the local target ref —
    /// recorded in task history alongside the transition it permitted.
    /// Persisted unresolved criteria, including a prior handoff's obligations,
    /// leave the task in `review` until an operator records them met.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn complete_landing_attempt(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
        handoff_id: &str,
        evidence: &str,
        history: &[TaskHistoryEntry],
        params: &mut TaskCoordinationCommitParams,
        effects: &mut ClaimCommitEffects,
    ) -> Result<(), OrbitError> {
        if evidence.trim().is_empty() {
            return Err(invalid("verified merge evidence required"));
        }
        if state.unresolved_merge_intent.is_some() {
            return Err(OrbitError::claim_refused(
                ClaimRefusalKind::UnresolvedMergeIntent,
            ));
        }
        // Full recheck: operator context, current authorization, no revocation,
        // trusted candidate observation and digest-pinned validation evidence.
        self.check_handoff_landing(auth, state)?;
        let (old, mut attempt) = self.live_attempt(auth, handoff_id)?;
        attempt.state = LandingAttemptState::Merged;
        attempt.evidence = Some(evidence.into());
        attempt.updated_at = Utc::now();
        effects
            .replacements
            .push((old, row(ATTEMPT, handoff_id, &attempt)?));
        self.settle_landing_request(handoff_id, LandingStartState::Completed, effects)?;
        let owes =
            PostDeployRecord::read(history).is_some_and(|record| !record.outstanding().is_empty());
        params.status = (!owes).then_some(TaskStatus::Done);
        params.status_note = Some(evidence.into());
        params.status_event = Some("landing_completed".into());
        Ok(())
    }

    /// Stop the live attempt with durable evidence. The task stays in `review`
    /// with its authorization intact; a repair is a new validated handoff.
    ///
    /// A `repairable` stop — the candidate conflicts with, or is stale
    /// against, its base [ORB-14261], or its pull request fails a hosted
    /// check the candidate caused [ORB-15956] — makes that new handoff
    /// automatic. The
    /// handoff's authority is revoked so nothing lands it, and the outcome
    /// tells the caller what becomes of the claim: an original claim waits in
    /// `repair_pending` with its task back `in-progress` for one repair leaf;
    /// a claim that already is that repair fails and blocks its task with
    /// both attempts' human reasons. The raw stop string, including an
    /// `owner_landing_refusal` diagnostic, stays on the landing attempt.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn stop_landing_attempt(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
        handoff_id: &str,
        reason: &str,
        repairable: bool,
        params: &mut TaskCoordinationCommitParams,
        effects: &mut ClaimCommitEffects,
    ) -> Result<LandingStop, OrbitError> {
        if !auth.operator || state.claim.phase != ExecutionClaimPhase::HandedOff {
            return Err(invalid("current operator landing context required"));
        }
        if reason.trim().is_empty() {
            return Err(invalid("stopping a landing requires evidence"));
        }
        if state.unresolved_merge_intent.is_some() {
            return Err(OrbitError::claim_refused(
                ClaimRefusalKind::UnresolvedMergeIntent,
            ));
        }
        let (old, mut attempt) = self.live_attempt(auth, handoff_id)?;
        let displayed = operator_stop_reason(reason);
        attempt.state = LandingAttemptState::Stopped;
        attempt.evidence = Some(reason.to_string());
        attempt.updated_at = Utc::now();
        effects
            .replacements
            .push((old, row(ATTEMPT, handoff_id, &attempt)?));
        params.status_note = Some(displayed.clone());
        params.status_event = Some("landing_stopped".into());
        if !repairable {
            return Ok(LandingStop::Stopped);
        }
        let accepted = self.accepted_handoff(&auth.claim_id)?;
        match &state.claim.repair {
            None => {
                self.revoke_handoff_authority(
                    auth,
                    &format!(
                        "landing stopped on a repairable failure; an automatic repair \
                         replaces this handoff: {displayed}"
                    ),
                    params,
                    effects,
                )?;
                params.status = Some(TaskStatus::InProgress);
                params.status_note = Some(format!(
                    "{displayed}; the candidate waits for one automatic repair leaf, which \
                     re-applies it onto the current base, revalidates it and hands it off again"
                ));
                Ok(LandingStop::Repair)
            }
            Some(first) => {
                self.revoke_handoff_authority(
                    auth,
                    &format!("the automatic repair's landing stopped again: {displayed}"),
                    params,
                    effects,
                )?;
                params.status = Some(TaskStatus::Blocked);
                Ok(LandingStop::Blocked(repair_exhausted_comment(
                    first,
                    &state.claim.claim_id,
                    &accepted,
                    &displayed,
                )))
            }
        }
    }
}

/// Operator text for a landing stop.
///
/// The landing attempt keeps the raw evidence string. An
/// `owner_landing_refusal` diagnostic stays there; status notes, revocation
/// reasons, the repair leaf's stop text and the blocked comment carry its
/// capped human `reason.value`. Any other evidence, including an older
/// plain string, is shown unchanged.
pub(super) fn operator_stop_reason(evidence: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(evidence) else {
        return evidence.to_string();
    };
    if value.get("kind").and_then(Value::as_str) != Some("owner_landing_refusal") {
        return evidence.to_string();
    }
    value
        .pointer("/reason/value")
        .and_then(Value::as_str)
        .filter(|reason| !reason.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| evidence.to_string())
}

fn retry_id(handoff_id: &str, request_key: &str) -> String {
    orbit_common::security::release::sha256_hex(format!("{handoff_id}\0{request_key}").as_bytes())
}

/// What a landing stop did to its claim.
pub(super) enum LandingStop {
    /// The claim stays handed off and the task in `review`.
    Stopped,
    /// The claim waits for one automatic repair leaf.
    Repair,
    /// The automatic repair is spent: the claim fails and the task blocks with
    /// this comment.
    Blocked(String),
}

/// The blocked task's comment: both attempts' candidates and the human
/// reason each stop recorded, so an operator sees why the automatic repair
/// could not land either. A diagnostic object stays on the attempt.
fn repair_exhausted_comment(
    first: &ClaimRepair,
    repair_claim_id: &str,
    repaired: &AcceptedHandoff,
    reason: &str,
) -> String {
    let first_reason = operator_stop_reason(&first.stop_evidence);
    let reason = operator_stop_reason(reason);
    format!(
        "Automatic repair exhausted: the repaired handoff's landing stopped again, so the \
         task is blocked for an operator.\n\n\
         First attempt (claim {}, handoff {}): {}\nStopped: {first_reason}\n\n\
         Repair attempt (claim {repair_claim_id}, handoff {}): {}\nStopped: {reason}",
        first.repairs_claim_id,
        first.handoff_id,
        describe_candidate(&first.candidate),
        repaired.handoff_id,
        describe_candidate(&repaired.handoff.candidate),
    )
}

fn describe_candidate(candidate: &HandoffCandidate) -> String {
    let delivery = match &candidate.delivery {
        HandoffDelivery::PullRequest { number } => format!("pull request #{number}"),
        HandoffDelivery::LocalCandidate => "owner-local candidate".to_string(),
        HandoffDelivery::NoDiff { .. } => "no-diff delivery".to_string(),
        HandoffDelivery::AlreadyLanded {
            covering_commit, ..
        } => format!("already landed in {covering_commit}"),
    };
    format!(
        "candidate {} on base {} from branch '{}', {delivery}",
        candidate.candidate.commit, candidate.base.commit, candidate.source_branch
    )
}
