use orbit_common::OrbitError;
use orbit_store::contracts::{ClaimInvocation, ClaimMutation, ClaimMutationResult, ClaimRun};
use orbit_types::tool::ToolSessionContext;

use crate::OrbitRuntime;
use crate::application::distributed::ensure_distributed_mutation_available;

/// The mutation ids a follower's retries replay under. One per claim and
/// operation, so a lost answer retried with the same claim returns the
/// recorded outcome instead of performing the write again, and a second,
/// different run can never be bound to the same claim.
fn bind_mutation_id(claim_id: &str) -> String {
    format!("pull-bind:{claim_id}")
}

impl OrbitRuntime {
    /// Bind the follower's one local leaf run to its claim, `claimed → running`.
    ///
    /// Idempotent per claim: the journal replays a repeated bind of the same
    /// run and refuses a different one, so a lost answer is safe to retry and a
    /// second leaf can never take over the attempt.
    pub fn serve_claim_bind(
        &self,
        session: &ToolSessionContext,
        claim_id: &str,
        run_id: &str,
        ship: orbit_store::contracts::AdmissionShipContract,
    ) -> Result<ClaimMutationResult, OrbitError> {
        ensure_distributed_mutation_available("orbit.drain.claim.bind")?;
        self.ensure_distributed_owner_workspace()?;
        let machine = self.session_caller_machine(session)?;
        let claim = self.current_claim(claim_id)?;
        let run_id = run_id.trim();
        if run_id.is_empty() {
            return Err(OrbitError::InvalidInput("`run_id` is required".into()));
        }
        // The invocation carries no run yet: binding is what creates that
        // association, so asserting one beforehand would fence the very
        // mutation being made.
        let context = ClaimInvocation::trusted_worker(
            claim.task_id.clone(),
            claim.claim_id.clone(),
            machine.clone(),
            None,
        );
        self.mutate_execution_claim(
            Some(&context),
            &bind_mutation_id(&claim.claim_id),
            &ClaimMutation::Bind {
                run: ClaimRun {
                    machine_id: machine,
                    run_id: run_id.to_string(),
                },
                ship,
            },
        )
    }
}
