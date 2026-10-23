use super::super::super::TaskCommitBoundary;
use super::super::codec::{CLAIM, STATE};
use super::super::{ClaimAuthority, decode, invalid};
use crate::contracts::*;
use orbit_common::{ClaimRefusalKind, OrbitError};
use orbit_types::task::TaskStatus;

impl TaskCommitBoundary {
    /// The claim `auth` acts under, refused as `stale_claim` unless it is the
    /// task's current, unsettled claim, held by the invoking machine and bound
    /// run, with the task in the status the claim's phase implies. Every claim
    /// mutation and every worker read fence stands on exactly these checks.
    /// `recovering` admits a failed claim for an operator's recovery.
    pub(in super::super) fn claim_authority(
        &self,
        auth: &ClaimInvocation,
        recovering: bool,
    ) -> Result<ClaimAuthority, OrbitError> {
        let row = self
            .coordination_row(CLAIM, &auth.claim_id)?
            .ok_or_else(|| OrbitError::claim_refused(ClaimRefusalKind::StaleClaim))?;
        let claim: ExecutionClaim = decode(&row.payload_json)?;
        let recover_failed =
            auth.operator && recovering && claim.phase == ExecutionClaimPhase::Failed;
        if claim.task_id != auth.task_id || !(claim.phase.is_unsettled() || recover_failed) {
            return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
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
            return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
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
            return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
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
            return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
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
            return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
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
                return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
            }
            Ok(())
        })
    }
}
