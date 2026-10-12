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
use orbit_store::contracts::{ExecutionClaim, LocalPullAdmission, LocalPullPhase};
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::{
    HostValidationGates, JobRunState, ValidationGateSource, ValidationGates,
};

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
    crate::runtime::host_resource::delivery::CLAIMED_LEAF_JOBS;

impl ClaimedLeaf {
    /// The owner's gates this claim was admitted under [ORB-15622]: the
    /// receipt's capture, or else the lists the ship contract's review
    /// captured. `None` for a claim an older owner admitted with neither.
    pub(crate) fn owner_gates(&self) -> Option<ValidationGates> {
        if let Some(gates) = self
            .admission
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.gates.clone())
        {
            return Some(gates);
        }
        let review = self.admission.request.ship.review.as_ref()?;
        Some(ValidationGates {
            required_validation_commands: review.required_validation_commands.clone()?,
            baseline_commands: review.baseline_commands.clone(),
        })
    }
}

impl OrbitRuntime {
    /// This host's own `workflow.required_validation_commands` and
    /// `review.baseline_commands`.
    pub(crate) fn local_validation_gates(&self) -> ValidationGates {
        ValidationGates {
            required_validation_commands: self.workflow_required_validation_commands().to_vec(),
            baseline_commands: self.review_baseline_commands().to_vec(),
        }
    }

    /// The gates this process holds a candidate to [ORB-15622]. A claimed
    /// leaf applies the owner's gates its claim captured. One whose claim
    /// captured none, from an older owner, falls back to this replica's
    /// configuration and warns, since that copy may have drifted from the
    /// owner's. Every other run applies this host's configuration.
    pub(crate) fn host_validation_gates(&self) -> HostValidationGates {
        if self.worker_invocation().is_none() {
            return HostValidationGates::local(self.local_validation_gates());
        }
        let owner = self.current_claimed_leaf().map(|leaf| leaf.owner_gates());
        self.claim_validation_gates(owner)
    }

    /// [`Self::host_validation_gates`] for a claimed leaf, given what its
    /// claim captured.
    pub(crate) fn claim_validation_gates(
        &self,
        owner: Result<Option<ValidationGates>, OrbitError>,
    ) -> HostValidationGates {
        match owner {
            Ok(Some(gates)) => HostValidationGates {
                gates,
                source: ValidationGateSource::OwnerContract,
            },
            missing => {
                let reason = match missing {
                    Err(error) => format!("its admission could not be read: {error}"),
                    _ => "its admission captured no owner gates (an older owner)".to_string(),
                };
                tracing::warn!(
                    "claimed leaf validates against this replica's \
                     `workflow.required_validation_commands` and `review.baseline_commands`, \
                     which may differ from the owner's: {reason}"
                );
                HostValidationGates {
                    gates: self.local_validation_gates(),
                    source: ValidationGateSource::ReplicaConfig,
                }
            }
        }
    }

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
        binding.validate()?;
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

    /// [ORB-14661] The claimed leaf this process executes, while it still
    /// holds its claim: `run_id` must be the leaf the binding names, its
    /// admission must agree with the binding, and that admission must be
    /// launched and not yet settling. A claim the executor has begun to
    /// settle, or that ended, authorizes no further read of its evidence.
    ///
    /// Each refusal leads with a reason token: `not_claimed_leaf`,
    /// `claim_unbound` or `claim_not_live`.
    pub(crate) fn live_claimed_leaf(&self, run_id: &str) -> Result<ClaimedLeaf, OrbitError> {
        if self
            .worker_invocation()
            .is_none_or(|binding| binding.bound_run_id != run_id)
        {
            return Err(refused(
                "not_claimed_leaf: the run is not the claimed leaf bound to this task",
            ));
        }
        let leaf = self.current_claimed_leaf().map_err(|error| {
            refused(format!(
                "claim_unbound: {}",
                match error {
                    OrbitError::PolicyDenied(reason) => reason,
                    other => other.to_string(),
                }
            ))
        })?;
        if !matches!(
            leaf.admission.phase,
            LocalPullPhase::Launching | LocalPullPhase::Launched
        ) {
            return Err(refused(
                "claim_not_live: the leaf's claim is settling, settled or released",
            ));
        }
        Ok(leaf)
    }

    /// [ORB-14661] [`Self::live_claimed_leaf`] while its run is running with
    /// final recovery admitted for the claimed task and not yet decided: the
    /// window in which the leaf's final-recovery agent runs. Refuses with
    /// `final_recovery_stale` outside it.
    pub(crate) fn claimed_leaf_in_final_recovery(
        &self,
        run_id: &str,
    ) -> Result<ClaimedLeaf, OrbitError> {
        let leaf = self.live_claimed_leaf(run_id)?;
        let running = self
            .stores()
            .jobs()
            .get_job_run(run_id)?
            .is_some_and(|run| run.state == JobRunState::Running);
        let deciding = self.read_run_state(run_id)?.is_some_and(|state| {
            state.final_recovery.is_some_and(|checkpoint| {
                checkpoint.key.run_id == run_id
                    && checkpoint.task_id == leaf.claim.task_id
                    && checkpoint.decision.is_none()
                    && checkpoint.outcome.is_none()
            })
        });
        if !running || !deciding {
            return Err(refused(
                "final_recovery_stale: the leaf's final recovery is not admitted, has already \
                 decided, or its run has ended",
            ));
        }
        Ok(leaf)
    }
}
