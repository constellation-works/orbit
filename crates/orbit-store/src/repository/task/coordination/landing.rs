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
use orbit_common::OrbitError;
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
        Ok(self
            .coordination_rows(ATTEMPT)?
            .into_iter()
            .find(|r| r.row_id == handoff_id))
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
            return Err(invalid("handoff identity mismatch"));
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
            return Err(invalid("handoff identity mismatch"));
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
                    return Err(invalid("handoff has already landed"));
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
            return Err(invalid("unresolved external merge intent"));
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
    pub(super) fn stop_landing_attempt(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
        handoff_id: &str,
        reason: &str,
        params: &mut TaskCoordinationCommitParams,
        effects: &mut ClaimCommitEffects,
    ) -> Result<(), OrbitError> {
        if !auth.operator || state.claim.phase != ExecutionClaimPhase::HandedOff {
            return Err(invalid("current operator landing context required"));
        }
        if reason.trim().is_empty() {
            return Err(invalid("stopping a landing requires evidence"));
        }
        if state.unresolved_merge_intent.is_some() {
            return Err(invalid("unresolved external merge intent"));
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
        Ok(())
    }
}
