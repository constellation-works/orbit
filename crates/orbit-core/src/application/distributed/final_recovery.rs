//! Owner-side application of a claimed leaf's final-recovery decision
//! [ORB-13907].
//!
//! A follower never writes its owner's task: its leaf's final recovery only
//! records a decision, and the leaf's failure settlement carries it here. The
//! owner applies it through the same deterministic applier a local run uses,
//! with its own checkout, its own base branch and review-level completion — a
//! claimed handoff never completes past review without the owner's approval,
//! so neither does a recovered one.

use orbit_store::contracts::{
    ClaimFinalRecovery, ClaimMutationResult, ExecutionClaim, ExecutionClaimPhase,
};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::FinalRecoveryDecision;

use crate::OrbitRuntime;
use crate::application::task::{
    FinalRecoveryCompletion, FinalRecoveryRequest, FinalRecoveryRequeueBound,
    FinalRecoveryTaskRevision,
};

impl OrbitRuntime {
    /// Apply `final_recovery` after `result` settled `claim` as failed.
    ///
    /// Best effort: the failure settlement already stands, and a decision this
    /// cannot apply leaves the task blocked exactly as an owner without the
    /// hook would. It applies only when the journal fenced the same run that
    /// decided, the claim is failed and the task is still the blocked task the
    /// failure left; the applier itself skips a run whose decision it already
    /// applied. A replayed settlement, or one an operator has already acted
    /// on, changes nothing.
    pub(super) fn apply_settled_final_recovery(
        &self,
        claim: &ExecutionClaim,
        run_id: Option<&str>,
        result: &ClaimMutationResult,
        final_recovery: &ClaimFinalRecovery,
    ) {
        if let Err(error) =
            self.try_apply_settled_final_recovery(claim, run_id, result, final_recovery)
        {
            tracing::warn!(
                target: "orbit.distributed.final_recovery",
                task_id = claim.task_id.as_str(),
                claim_id = claim.claim_id.as_str(),
                run_id = final_recovery.run_id.as_str(),
                error = %error,
                "claimed leaf final recovery was not applied; the task stays blocked",
            );
        }
    }

    fn try_apply_settled_final_recovery(
        &self,
        claim: &ExecutionClaim,
        run_id: Option<&str>,
        result: &ClaimMutationResult,
        final_recovery: &ClaimFinalRecovery,
    ) -> Result<(), orbit_common::OrbitError> {
        let fenced_run = run_id.map(str::trim) == Some(final_recovery.run_id.as_str());
        if !fenced_run
            || result.phase != ExecutionClaimPhase::Failed
            || matches!(
                final_recovery.decision,
                FinalRecoveryDecision::Resume { .. }
            )
        {
            return Ok(());
        }
        let task = self.get_task(&claim.task_id)?;
        if task.status != TaskStatus::Blocked {
            return Ok(());
        }
        let request = FinalRecoveryRequest {
            task_id: claim.task_id.clone(),
            run_id: final_recovery.run_id.clone(),
            observed: FinalRecoveryTaskRevision::of(&task),
            repo_root: self.context.paths().repo_root.clone(),
            base_ref: self.owner_ship_contract().base_branch,
            completion: FinalRecoveryCompletion::Review,
            requeue_bound: FinalRecoveryRequeueBound::default(),
        };
        let output = serde_json::to_value(&final_recovery.decision).map_err(|error| {
            orbit_common::OrbitError::Execution(format!("encode decision: {error}"))
        })?;
        let outcome = self.apply_final_recovery(&request, Some(&output))?;
        tracing::info!(
            target: "orbit.distributed.final_recovery",
            task_id = claim.task_id.as_str(),
            claim_id = claim.claim_id.as_str(),
            run_id = final_recovery.run_id.as_str(),
            outcome = ?outcome,
            "claimed leaf final recovery applied",
        );
        Ok(())
    }
}
