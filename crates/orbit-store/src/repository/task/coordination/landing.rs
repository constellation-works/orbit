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
use orbit_types::task::TaskStatus;
use orbit_types::workflow::handoff::*;

use super::TaskCommitBoundary;
use super::lifecycle::{decode, invalid, row};
use crate::contracts::*;

const ATTEMPT: &str = "distributed-landing-attempt-v1";

impl TaskCommitBoundary {
    /// Every recorded attempt, newest state included. Read-only inspection for
    /// the owner consumer and its operators.
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
    /// first one, or the next after a stop or an owner job that died. `Some`
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
                    // A stopped attempt reopens deliberately, and so does one
                    // whose job is gone. The candidate, evidence and authority
                    // are rechecked before any merge intent, so a stale
                    // candidate stops again instead of merging unvalidated work.
                    None => {
                        if attempt.state != LandingAttemptState::Dispatched
                            || attempt.job_run_id.is_some()
                        {
                            attempt.attempt = attempt.attempt.saturating_add(1);
                            attempt.state = LandingAttemptState::Dispatched;
                            attempt.job_run_id = None;
                            attempt.evidence = None;
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
    pub(super) fn complete_landing_attempt(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
        handoff_id: &str,
        evidence: &str,
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
        params.status = Some(TaskStatus::Done);
        params.status_note = Some(evidence.into());
        params.status_event = Some("landing_completed".into());
        Ok(())
    }

    /// Stop the live attempt with durable evidence. The task stays in `review`
    /// with its authorization intact; a repair is a new validated handoff.
    ///
    /// A `repairable` stop — the candidate conflicts with, or is stale
    /// against, its base — makes that new handoff automatic [ORB-14261]. The
    /// handoff's authority is revoked so nothing lands it, and the outcome
    /// tells the caller what becomes of the claim: an original claim waits in
    /// `repair_pending` with its task back `in-progress` for one repair leaf;
    /// a claim that already is that repair fails and blocks its task with
    /// both attempts' evidence.
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
        attempt.state = LandingAttemptState::Stopped;
        attempt.evidence = Some(reason.into());
        attempt.updated_at = Utc::now();
        effects
            .replacements
            .push((old, row(ATTEMPT, handoff_id, &attempt)?));
        params.status_note = Some(reason.into());
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
                        "landing stopped on a conflicting or stale base; an automatic repair \
                         replaces this handoff: {reason}"
                    ),
                    params,
                    effects,
                )?;
                params.status = Some(TaskStatus::InProgress);
                params.status_note = Some(format!(
                    "{reason}; the candidate waits for one automatic repair leaf, which \
                     re-applies it onto the current base, revalidates it and hands it off again"
                ));
                Ok(LandingStop::Repair)
            }
            Some(first) => {
                self.revoke_handoff_authority(
                    auth,
                    &format!("the automatic repair's landing stopped on its base again: {reason}"),
                    params,
                    effects,
                )?;
                params.status = Some(TaskStatus::Blocked);
                Ok(LandingStop::Blocked(repair_exhausted_comment(
                    first,
                    &state.claim.claim_id,
                    &accepted,
                    reason,
                )))
            }
        }
    }
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

/// The blocked task's comment: both attempts' candidates and stop evidence,
/// so an operator sees why the automatic repair could not land either.
fn repair_exhausted_comment(
    first: &ClaimRepair,
    repair_claim_id: &str,
    repaired: &AcceptedHandoff,
    reason: &str,
) -> String {
    format!(
        "Automatic repair exhausted: the repaired handoff's landing stopped on its base \
         again, so the task is blocked for an operator.\n\n\
         First attempt (claim {}, handoff {}): {}\nStopped: {}\n\n\
         Repair attempt (claim {repair_claim_id}, handoff {}): {}\nStopped: {reason}",
        first.repairs_claim_id,
        first.handoff_id,
        describe_candidate(&first.candidate),
        first.stop_evidence,
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
