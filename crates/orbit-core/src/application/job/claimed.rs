//! Trusted execution context for a claimed distributed leaf [ORB-12616].
//!
//! Two independent facts must agree before a claimed leaf may do anything:
//!
//! 1. the **process worker binding** — resolved by `OrbitRuntime::open` from
//!    the host-only authority record this process (or a live ancestor) was
//!    registered under, and required outright when the launcher set
//!    `ORBIT_WORKER_CONTEXT_REQUIRED`; and
//! 2. the **durable pull admission** — the owner's receipt, claim and unique
//!    leaf binding, written before the run existed.
//!
//! Neither is a payload. Job input, activity input and environment can only
//! ever be compared against what these two say, which is why a claimed leaf
//! cannot be started, redirected or impersonated by editing a run's input.

use orbit_common::OrbitError;
use orbit_store::contracts::{ExecutionClaim, LocalPullAdmission};
use orbit_types::tool::WorkerInvocation;

use crate::OrbitRuntime;

fn refused(message: impl Into<String>) -> OrbitError {
    OrbitError::PolicyDenied(message.into())
}

/// One claimed leaf's admission, already checked against the trusted binding.
pub(crate) struct ClaimedLeaf {
    pub(crate) admission: LocalPullAdmission,
    pub(crate) claim: ExecutionClaim,
    pub(crate) binding: WorkerInvocation,
}

/// Leaf definitions that only a claim may run. A run on one of these without
/// a durable admission is a mis-dispatch, not a workload: every step below
/// `worktree` reads claim context that will never resolve.
pub(crate) const CLAIMED_LEAF_JOBS: &[&str] =
    &["task_claimed_local_pipeline", "task_claimed_pr_pipeline"];

impl OrbitRuntime {
    /// The durable admission for a leaf run, if that run is a claimed leaf.
    pub(crate) fn claimed_leaf_admission(
        &self,
        run_id: &str,
    ) -> Result<Option<LocalPullAdmission>, OrbitError> {
        self.stores().jobs().local_pull_for_run(run_id)
    }

    /// Authorize this process to execute `run_id` as its claimed leaf.
    ///
    /// The refusal is deliberately the same shape whether no binding resolved
    /// or a binding resolved for different work: an unbound generic worker and
    /// a worker bound to somebody else's claim are both "not this claim's
    /// executor", and neither may proceed.
    pub(crate) fn authorize_claimed_leaf(
        &self,
        run_id: &str,
        admission: LocalPullAdmission,
    ) -> Result<ClaimedLeaf, OrbitError> {
        let binding = self.worker_invocation().cloned().ok_or_else(|| {
            refused(
                "claimed leaves require the internal handoff execution adapter; generic pipeline \
                 execution is unavailable without a trusted worker binding",
            )
        })?;
        binding.validate().map_err(OrbitError::InvalidInput)?;
        let claim = admission
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.claim.clone())
            .ok_or_else(|| refused("claimed leaf has no admitted claim"))?;
        if admission.leaf_run_id.as_deref() != Some(run_id) {
            return Err(refused("claimed leaf run binding mismatch"));
        }
        for (field, bound, admitted) in [
            ("run", binding.bound_run_id.as_str(), run_id),
            ("claim", binding.claim_id.as_str(), claim.claim_id.as_str()),
            ("task", binding.task_id.as_str(), claim.task_id.as_str()),
            (
                "execution machine",
                binding.execution.machine_id.as_str(),
                claim.executed_on.machine_id.as_str(),
            ),
            (
                "owner machine",
                binding.owner_machine_id.as_str(),
                admission.destination.owner_machine_id.as_str(),
            ),
            (
                "owner workspace",
                binding.owner_workspace_id.as_str(),
                admission.destination.owner_workspace_id.as_str(),
            ),
        ] {
            if bound != admitted {
                return Err(refused(format!(
                    "claimed leaf {field} binding mismatch: this worker is bound to '{bound}', \
                     the admission records '{admitted}'"
                )));
            }
        }
        Ok(ClaimedLeaf {
            admission,
            claim,
            binding,
        })
    }

    /// The claimed leaf this process is executing, resolved from its binding.
    pub(crate) fn current_claimed_leaf(&self) -> Result<ClaimedLeaf, OrbitError> {
        let binding = self.worker_invocation().cloned().ok_or_else(|| {
            refused("claimed execution requires a trusted worker binding; none resolved")
        })?;
        let run_id = binding.bound_run_id.clone();
        let admission = self.claimed_leaf_admission(&run_id)?.ok_or_else(|| {
            refused(format!(
                "no durable pull admission binds run '{run_id}'; this process is not executing a \
                 claimed leaf"
            ))
        })?;
        self.authorize_claimed_leaf(&run_id, admission)
    }
}
