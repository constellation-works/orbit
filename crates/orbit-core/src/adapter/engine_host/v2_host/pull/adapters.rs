//! Owner, routed-owner and launcher adapters for the pull drain
//! [ORB-12616, ORB-13625].
//!
//! [`super::drain::PullDrain`] was proven against injected doubles when the
//! foundation landed; these are the production implementations of its seams.
//!
//! - **Owner-local** (`OwnerPullPeer`, test-only): owner and executor are the
//!   same machine and workspace, so admission runs on this owner's commit
//!   boundary and binding and settlement on this owner's claim journal.
//! - **Follower** ([`RoutedPullPeer`]): the owner is another machine. Every
//!   call goes over the composition-supplied [`DrainOwnerTransport`] to the
//!   owner's registered `orbit.task.pull`, `orbit.drain.claim.bind`,
//!   `orbit.drain.claim.settle` and `orbit.drain.receipt.lookup` tools, which
//!   run the same owner code the owner-local adapter calls directly. There is
//!   no local fallback: a missing transport refuses.
//!
//! Nothing here reads authority from a payload. The destination is caller-side
//! durable identity built by the drain from its runtime; on the owner, each
//! claim mutation carries a [`ClaimInvocation`] the store fences task, machine,
//! bound run and phase against inside its own transaction, with the machine
//! taken from the trusted session rather than from anything a follower sends.

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_store::contracts::{
    AdmissionLookup, AdmissionReceipt, AdmissionRequest, ClaimMutation, ExecutionClaim,
    LocalPullAdmission, PullDestination,
};
use orbit_tools::DrainOwnerTransport;
use orbit_types::tool::WorkerInvocation;
use serde_json::{Value, json};

use super::drain::{PullLauncher, PullPeer};
use crate::OrbitRuntime;

fn refused(message: impl Into<String>) -> OrbitError {
    OrbitError::PolicyDenied(message.into())
}

/// The claim an admission record is acting on.
fn admitted_claim(admission: &LocalPullAdmission) -> Result<ExecutionClaim, OrbitError> {
    admission
        .receipt
        .as_ref()
        .and_then(|receipt| receipt.claim.clone())
        .ok_or_else(|| refused("this admission holds no claim to act on"))
}

/// The follower half of the pull protocol: every call is delivered to the
/// owner's registered tools over the composition-supplied transport.
///
/// The owner resolves the calling machine from its trusted session, so
/// nothing sent here names a machine; the claim and run IDs only say which
/// attempt is being carried forward, and the owner's claim journal refuses any
/// attempt that is not this machine's current one.
pub(crate) struct RoutedPullPeer {
    pub(crate) transport: Arc<dyn DrainOwnerTransport>,
}

impl RoutedPullPeer {
    fn call(
        &self,
        destination: &PullDestination,
        tool: &str,
        input: Value,
    ) -> Result<Value, OrbitError> {
        self.transport.call(&destination.selector, tool, input)
    }
}

fn decode<T: serde::de::DeserializeOwned>(value: Value, what: &str) -> Result<T, OrbitError> {
    serde_json::from_value(value)
        .map_err(|error| OrbitError::Store(format!("owner {what} response: {error}")))
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Value, OrbitError> {
    serde_json::to_value(value).map_err(|error| OrbitError::Store(error.to_string()))
}

impl PullPeer for RoutedPullPeer {
    fn request(
        &self,
        destination: &PullDestination,
        request: &AdmissionRequest,
    ) -> Result<AdmissionReceipt, OrbitError> {
        let response = self.call(destination, "orbit.task.pull", encode(request)?)?;
        let receipt = response
            .get("receipt")
            .cloned()
            .ok_or_else(|| OrbitError::Store("owner pull response carries no receipt".into()))?;
        decode(receipt, "pull")
    }

    fn bind(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        let claim = admitted_claim(admission)?;
        let run_id = admission
            .leaf_run_id
            .clone()
            .ok_or_else(|| refused("binding requires a created leaf run"))?;
        self.call(
            &admission.destination,
            "orbit.drain.claim.bind",
            json!({
                "claim_id": claim.claim_id,
                "run_id": run_id,
                "ship": encode(&admission.request.ship)?,
            }),
        )?;
        Ok(())
    }

    fn settle(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        let claim = admitted_claim(admission)?;
        let settlement = admission
            .settlement
            .as_ref()
            .ok_or_else(|| refused("settlement was not persisted before the owner call"))?;
        if !matches!(
            settlement,
            ClaimMutation::Fail(_) | ClaimMutation::AcceptHandoff(_)
        ) {
            return Err(refused(
                "only a typed handoff or a failure settles a claimed leaf",
            ));
        }
        self.call(
            &admission.destination,
            "orbit.drain.claim.settle",
            json!({
                "claim_id": claim.claim_id,
                "run_id": admission.leaf_run_id,
                "settlement": encode(settlement)?,
            }),
        )?;
        Ok(())
    }

    fn lookup(
        &self,
        destination: &PullDestination,
        request_id: &str,
    ) -> Result<AdmissionLookup, OrbitError> {
        let response = self.call(
            destination,
            "orbit.drain.receipt.lookup",
            json!({ "request_id": request_id }),
        )?;
        match response.get("outcome").and_then(Value::as_str) {
            Some("found") => {
                let receipt = response.get("receipt").cloned().ok_or_else(|| {
                    OrbitError::Store("owner lookup found a receipt but returned none".into())
                })?;
                let current_claim = match response.get("current_claim") {
                    None | Some(Value::Null) => None,
                    Some(claim) => Some(Box::new(decode(claim.clone(), "lookup")?)),
                };
                Ok(AdmissionLookup::Found {
                    receipt: Box::new(decode(receipt, "lookup")?),
                    current_claim,
                })
            }
            Some("expired") => Ok(AdmissionLookup::Expired),
            Some("not_found") => Ok(AdmissionLookup::NotFound),
            other => Err(OrbitError::Store(format!(
                "owner receipt lookup answered an unknown outcome {other:?}"
            ))),
        }
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
pub(crate) struct LeafPullLauncher<'a> {
    pub(crate) runtime: &'a OrbitRuntime,
}

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
        let coordinator = self.coordinator(&admission.destination)?;
        self.runtime
            .clone()
            .with_worker_invocation(invocation, coordinator)
    }

    /// Where the bound worker's coordination goes: this process when it is
    /// the owner, otherwise the composition-supplied route to the remote
    /// owner. A follower without that route refuses rather than coordinating
    /// against its own replica store.
    fn coordinator(
        &self,
        destination: &PullDestination,
    ) -> Result<Arc<dyn orbit_tools::OwnerCoordinator>, OrbitError> {
        if self.runtime.automation_machine_identity() == Some(destination.owner_machine_id.as_str())
        {
            return Ok(Arc::new(LocalOwnerCoordinator {
                runtime: self.runtime.without_worker_routing(),
            }));
        }
        self.runtime
            .drain_owner_transport()
            .map(|transport| transport.worker_coordinator())
            .ok_or_else(|| {
                refused(format!(
                    "owner '{}' is another machine and this runtime has no route to it",
                    destination.owner_machine_id
                ))
            })
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

    /// Cancel a cancelled drain's queued leaf through the ordinary run
    /// cancellation, so it is audited like any other and its terminalization
    /// records the claim's failure settlement [ORB-13663].
    fn cancel_queued(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        let run_id = admission
            .leaf_run_id
            .as_deref()
            .ok_or_else(|| refused("cancelling a queued leaf requires a created leaf run"))?;
        self.runtime.cancel_job_run_with_reason(
            run_id,
            "pull_drain",
            "pull_drain_abandon",
            Some("its follower drain is no longer running and never launched it"),
        )?;
        Ok(())
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
