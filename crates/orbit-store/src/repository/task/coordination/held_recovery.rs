//! Match a prospective operator admission without rewriting its source claim.
use orbit_common::{OrbitError, security::release::sha256_hex};
use orbit_types::task::{Task, TaskHistoryEntry};
use orbit_types::workflow::{
    HELD_RECOVERY_ADMITTED, HELD_RECOVERY_CLAIMED, HeldRecoveryAdmission, HeldRecoveryMeaning,
};

use super::TaskCommitBoundary;
use crate::contracts::{ClaimCandidateRef, ClaimInspection};

/// Stable digest of the source claim; elapsed display age is not authority.
fn recovery_claim_digest(claim: &ClaimInspection) -> Result<String, OrbitError> {
    let mut claim = claim.clone();
    claim.age_seconds = None;
    serde_json::to_vec(&claim)
        .map(|bytes| sha256_hex(&bytes))
        .map_err(|error| OrbitError::Store(error.to_string()))
}

impl TaskCommitBoundary {
    /// Digest the exact immutable source claim without its elapsed display age.
    pub fn recovery_claim_digest(claim: &ClaimInspection) -> Result<String, OrbitError> {
        recovery_claim_digest(claim)
    }

    /// A pending admission is fail-closed: it cannot turn into a fresh
    /// implementation after its task meaning or source settlement changes.
    pub(super) fn recovery_candidate(
        &self,
        task: &Task,
        history: &[TaskHistoryEntry],
    ) -> Result<Option<ClaimCandidateRef>, OrbitError> {
        let Some(entry) = history
            .iter()
            .rev()
            .find(|e| e.event == HELD_RECOVERY_ADMITTED)
        else {
            return Ok(None);
        };
        if entry.by != "system" {
            return Err(OrbitError::CapabilityDenied(
                "untrusted recovery admission".into(),
            ));
        }
        let admission: HeldRecoveryAdmission =
            serde_json::from_str(entry.note.as_deref().unwrap_or_default())
                .map_err(|e| OrbitError::Store(format!("unreadable recovery admission: {e}")))?;
        if history
            .iter()
            .any(|e| e.event == HELD_RECOVERY_CLAIMED && e.at >= entry.at)
        {
            return Ok(None);
        }
        if history
            .iter()
            .any(|e| e.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT && e.at >= entry.at)
        {
            return Ok(None);
        }
        let binding = &admission.binding;
        let refused = || {
            OrbitError::CapabilityDenied(
                "held recovery task, source, scope or discard binding changed".into(),
            )
        };
        let claim = self
            .execution_claim_state(&binding.claim_id)?
            .ok_or_else(refused)?;
        let preserved = claim.preserved_candidate.as_ref().ok_or_else(refused)?;
        if binding.schema_version != 1
            || binding.workspace_id != self.workspace_id
            || binding.task_id != task.id
            || binding.proposed != HeldRecoveryMeaning::from_task(task)
            || binding.proposed_spec_digest != task.spec_digest()
            || recovery_claim_digest(&claim)? != binding.claim_digest
            || claim.claim.phase.is_unsettled()
            || claim.unresolved_merge_intent.is_some()
            || claim.claim.task_id != task.id
            || claim.claim.executed_on.machine_id != binding.machine_id
            || claim.bound_run.as_ref().map(|r| r.run_id.as_str())
                != Some(binding.source_run_id.as_str())
            || preserved.task_spec_digest != binding.original_spec_digest
            || preserved.candidate.head_sha != binding.candidate_sha
            || preserved.candidate.pull_request.as_deref() != Some(binding.pull_request.as_str())
            || preserved.candidate.source_run_id.as_deref() != Some(binding.source_run_id.as_str())
        {
            return Err(refused());
        }
        let mut candidate = preserved.candidate.clone();
        // The reviewed head must also be on the existing PR branch. A hold's
        // separate evidence branch never authorizes replacing another PR tip.
        candidate.branch = binding.branch.clone();
        candidate.pull_request_head = Some(binding.branch.clone());
        candidate.published = true;
        candidate.held_recovery = Some(admission);
        Ok(Some(candidate))
    }
}
