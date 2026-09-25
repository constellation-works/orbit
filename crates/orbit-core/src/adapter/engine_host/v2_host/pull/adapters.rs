//! Real owner and launcher adapters for the internal pull drain [ORB-12616].
//!
//! [`super::drain::PullDrain`] was proven against injected doubles when the
//! foundation landed; these are the production implementations of the same two
//! seams, and the doubles now exist only in tests.
//!
//! What "real" means here, and where it stops:
//!
//! - **Owner-local is served end to end, for both delivery shapes.** Owner and
//!   executor are the same machine and workspace, so admission runs on this
//!   owner's commit boundary and binding and settlement run on this owner's
//!   claim journal. A local candidate is observed from this owner's checkout;
//!   a published pull request is observed from the provider *and* this
//!   checkout [ORB-12500].
//! - **A follower destination is refused, not faked.** Routed distributed
//!   mutations are still behind [`ensure_distributed_mutation_available`]: the
//!   routed peer that would speak this protocol over the federated transport
//!   does not exist, and no mutating distributed entry point is a registered
//!   tool. A follower drain therefore fails with that gate's message rather
//!   than silently pretending to reach an owner.
//!
//! Nothing here reads authority from a payload. The destination is caller-side
//! durable identity built by the drain from its runtime; each claim mutation
//! carries a [`ClaimInvocation`] the store fences task, machine, bound run and
//! phase against inside its own transaction.

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_store::TaskCommitBoundary;
use orbit_store::contracts::{
    AdmissionIdentity, AdmissionLookup, AdmissionReceipt, AdmissionRequest, ClaimInvocation,
    ClaimMutation, ClaimRun, ExecutionClaim, HandoffObservation, LocalPullAdmission,
    PullDestination,
};
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::task::ExecutionLocation;
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::handoff::{HandoffDelivery, TaskHandoff};

use super::drain::{PullLauncher, PullPeer};
use crate::OrbitRuntime;
use crate::application::distributed::{
    ensure_distributed_mutation_available, owner_binary_version,
};

fn refused(message: impl Into<String>) -> OrbitError {
    OrbitError::PolicyDenied(message.into())
}

/// The owner half of the pull protocol, served from this process.
#[allow(dead_code)]
pub(crate) struct OwnerPullPeer<'a> {
    pub(crate) runtime: &'a OrbitRuntime,
}

#[allow(dead_code)]
impl OwnerPullPeer<'_> {
    /// Confirm this process may serve `destination` at all.
    ///
    /// Only an owner-local destination is served here. The check is against
    /// the runtime's own registered machine and workspace, so a destination
    /// record naming this machine does not make a foreign owner local.
    fn ensure_owner_local(&self, destination: &PullDestination) -> Result<(), OrbitError> {
        let machine = self.runtime.automation_machine_identity().ok_or_else(|| {
            refused("this host has no registered machine identity; it cannot serve an admission")
        })?;
        if destination.owner_machine_id != machine || destination.execution_machine_id != machine {
            // Reaching a different machine is routed distributed mutation,
            // which this adapter does not implement. Report the gate's reason
            // while it is closed, and stay a refusal after it opens: a remote
            // destination never becomes owner-local, whatever the gate says.
            ensure_distributed_mutation_available("orbit.task.pull")?;
            return Err(refused(format!(
                "destination owner '{}' / executor '{}' is not this machine '{machine}'; the \
                 owner-local adapter serves only its own machine",
                destination.owner_machine_id, destination.execution_machine_id
            )));
        }
        if destination.owner_workspace_id != self.runtime.workspace_id()? {
            return Err(refused(format!(
                "destination workspace '{}' is not this owner's workspace",
                destination.owner_workspace_id
            )));
        }
        Ok(())
    }

    fn boundary(&self) -> Result<TaskCommitBoundary, OrbitError> {
        TaskCommitBoundary::new(
            self.runtime.sqlite_store()?,
            TaskRegistryStore::open(&task_registry_path(&self.runtime.global_root()))?,
            self.runtime.workspace_id()?,
        )
    }

    /// The claim an admission record is settling, with the destination checks
    /// already applied.
    fn claim(&self, admission: &LocalPullAdmission) -> Result<ExecutionClaim, OrbitError> {
        self.ensure_owner_local(&admission.destination)?;
        admission
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.claim.clone())
            .ok_or_else(|| refused("this admission holds no claim to act on"))
    }

    /// Trusted worker context for one claim mutation. The drain speaks for the
    /// executor it launched, never as an operator: recovery and approval stay
    /// out of reach of every automatic path.
    fn context(&self, claim: &ExecutionClaim, run: Option<ClaimRun>) -> ClaimInvocation {
        ClaimInvocation::trusted_worker(
            claim.task_id.clone(),
            claim.claim_id.clone(),
            claim.executed_on.machine_id.clone(),
            run,
        )
    }

    /// The owner's own reading of the candidate a claim is settling.
    ///
    /// Read from the owner checkout — and, for a published delivery, from the
    /// provider — with the shared observation rules, so the worker's handoff
    /// payload contributes nothing but the identity to look *at*. Anything
    /// that disagrees is refused by the claim journal, which compares this
    /// observation against the submitted candidate.
    ///
    /// Already-landed delivery keeps its refusal: no-diff work carries the
    /// existing typed already-landed report through its own verifier and is
    /// not a route a claimed leaf takes.
    fn observe(&self, handoff: &TaskHandoff) -> Result<HandoffObservation, OrbitError> {
        let candidate = match handoff.candidate.delivery {
            HandoffDelivery::LocalCandidate => orbit_engine::observe_candidate(
                &self.runtime.paths().repo_root,
                Some(&handoff.candidate.source_branch),
                &handoff.candidate.base_branch,
                &handoff.candidate.landing_branch,
                HandoffDelivery::LocalCandidate,
                &handoff.workspace_id,
                // An owner-local candidate has no origin to fetch and must
                // keep reading the local base it was synchronized onto.
                "local",
            )?,
            // [ORB-12500] The owner reads the published pull request itself:
            // the provider names the delivery, and the candidate and base
            // objects are resolved in this checkout.
            HandoffDelivery::PullRequest { .. } => orbit_engine::observe_published_candidate(
                self.runtime,
                &self.runtime.paths().repo_root,
                &handoff.candidate,
            )?,
            HandoffDelivery::AlreadyLanded { .. } => {
                return Err(refused(
                    "already-landed delivery carries its own typed report through the no-diff \
                     verifier; a claimed leaf does not hand one off",
                ));
            }
        };
        let required_commands = self
            .runtime
            .workflow_required_validation_commands()
            .to_vec();
        if required_commands.is_empty() {
            return Err(refused(
                "this owner declares no required validation commands \
                 (`workflow.required_validation_commands`), so no handoff can be accepted",
            ));
        }
        Ok(HandoffObservation {
            candidate,
            required_commands,
        })
    }
}

impl PullPeer for OwnerPullPeer<'_> {
    fn request(
        &self,
        destination: &PullDestination,
        request: &AdmissionRequest,
    ) -> Result<AdmissionReceipt, OrbitError> {
        self.ensure_owner_local(destination)?;
        let identity = AdmissionIdentity::trusted_local(ExecutionLocation {
            machine_id: destination.execution_machine_id.clone(),
            machine_name: None,
        });
        match self.boundary()?.admit_task(
            &identity,
            request,
            owner_binary_version(),
            &self.runtime.paths().repo_root,
            &self.runtime.data_root(),
        )? {
            AdmissionLookup::Found { receipt, .. } => Ok(*receipt),
            AdmissionLookup::Expired => Err(OrbitError::InvalidInput("request_expired".into())),
            AdmissionLookup::NotFound => Err(OrbitError::Store(
                "admission committed no receipt for this request".into(),
            )),
        }
    }

    fn bind(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        let claim = self.claim(admission)?;
        let run_id = admission
            .leaf_run_id
            .clone()
            .ok_or_else(|| refused("binding requires a created leaf run"))?;
        let run = ClaimRun {
            machine_id: claim.executed_on.machine_id.clone(),
            run_id,
        };
        // Idempotent by construction: the mutation id is the claim's, so a
        // replayed bind returns its recorded outcome and can never substitute
        // a different run for the same claim. The invocation carries no run
        // yet — binding is what creates that association, so asserting it
        // beforehand would fence the very mutation being made.
        self.runtime.mutate_execution_claim(
            Some(&self.context(&claim, None)),
            &format!("pull-bind:{}", claim.claim_id),
            &ClaimMutation::Bind {
                run,
                ship: admission.request.ship.clone(),
            },
        )?;
        Ok(())
    }

    fn settle(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        let claim = self.claim(admission)?;
        let settlement = admission
            .settlement
            .clone()
            .ok_or_else(|| refused("settlement was not persisted before the owner call"))?;
        let run = admission.leaf_run_id.clone().map(|run_id| ClaimRun {
            machine_id: claim.executed_on.machine_id.clone(),
            run_id,
        });
        let context = self.context(&claim, run);
        match settlement {
            ClaimMutation::Fail(evidence) => {
                self.runtime.mutate_execution_claim(
                    Some(&context),
                    &format!("pull-fail:{}", claim.claim_id),
                    &ClaimMutation::Fail(evidence),
                )?;
            }
            ClaimMutation::AcceptHandoff(handoff) => {
                let observation = self.observe(&handoff)?;
                self.runtime.accept_task_handoff(
                    &context,
                    &format!("pull-handoff:{}", claim.claim_id),
                    handoff,
                    observation,
                )?;
            }
            _ => {
                return Err(refused(
                    "only a typed handoff or a failure settles a claimed leaf",
                ));
            }
        }
        Ok(())
    }
}

/// Launches the one leaf run a claim is bound to, as that claim's worker.
///
/// The launcher does not create, choose or re-target a run: the admission
/// transaction already created exactly one, and [`bound_runtime`] refuses
/// anything else. What it adds is the trusted binding — recorded against the
/// child's PID by the existing supervisor, which also sets
/// `ORBIT_WORKER_CONTEXT_REQUIRED` so a child that cannot resolve it refuses
/// to run rather than falling back to an unauthenticated local identity.
///
/// [`bound_runtime`]: LeafPullLauncher::bound_runtime
#[allow(dead_code)]
pub(crate) struct LeafPullLauncher<'a> {
    pub(crate) runtime: &'a OrbitRuntime,
}

#[allow(dead_code)]
impl LeafPullLauncher<'_> {
    /// This runtime, bound to the admission's claim and leaf run.
    pub(crate) fn bound_runtime(
        &self,
        admission: &LocalPullAdmission,
    ) -> Result<OrbitRuntime, OrbitError> {
        let claim = admission
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.claim.clone())
            .ok_or_else(|| refused("launching requires an admitted claim"))?;
        let bound_run_id = admission
            .leaf_run_id
            .clone()
            .ok_or_else(|| refused("launching requires a bound leaf run"))?;
        let invocation = WorkerInvocation {
            owner_machine_id: admission.destination.owner_machine_id.clone(),
            owner_workspace_id: admission.destination.owner_workspace_id.clone(),
            owner_destination: admission.destination.selector.clone(),
            task_id: claim.task_id.clone(),
            claim_id: claim.claim_id.clone(),
            execution: claim.executed_on.clone(),
            bound_run_id,
        };
        let coordinator: Arc<dyn orbit_tools::OwnerCoordinator> = Arc::new(LocalOwnerCoordinator {
            runtime: self.runtime.without_worker_routing(),
        });
        self.runtime
            .clone()
            .with_worker_invocation(invocation, coordinator)
    }
}

impl PullLauncher for LeafPullLauncher<'_> {
    fn launch(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        let bound = self.bound_runtime(admission)?;
        let run_id = bound
            .worker_invocation()
            .map(|binding| binding.bound_run_id.clone())
            .ok_or_else(|| refused("bound runtime lost its worker invocation"))?;
        bound.spawn_claimed_leaf_worker(&run_id)
    }
}

/// Owner transport for a drain whose owner is this process.
///
/// The bound parent clone needs a coordinator to exist; in owner-local mode
/// the owner is reachable without a network hop, so routed coordination calls
/// execute against this same workspace under the worker's own capability.
struct LocalOwnerCoordinator {
    runtime: OrbitRuntime,
}

impl orbit_tools::OwnerCoordinator for LocalOwnerCoordinator {
    fn call(
        &self,
        name: &str,
        mut input: serde_json::Value,
        mut session: orbit_types::tool::ToolSessionContext,
    ) -> Result<serde_json::Value, OrbitError> {
        let binding = session
            .worker_invocation
            .clone()
            .ok_or_else(|| refused("owner coordination requires a worker binding"))?;
        // The routed call carries the host-qualified selector; this owner
        // addresses its own workspace by id, exactly as the remote transport's
        // local branch does.
        if let Some(object) = input.as_object_mut() {
            object.insert(
                "workspace".into(),
                serde_json::Value::String(binding.owner_workspace_id.clone()),
            );
        }
        session.workspace = Some(binding.owner_workspace_id.clone());
        self.runtime
            .execute_owner_coordination(name, input, session)
    }
}
