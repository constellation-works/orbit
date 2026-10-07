//! Compose a runtime with the existing owner MCP transport: a managed
//! worker's coordination route, and a follower drain's route to its owner.
use std::path::PathBuf;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_mcp::McpHost;
use orbit_mcp::federated::{self, FederatedMcpHost, SshDestinationProbe};
use orbit_tools::{DrainOwnerTransport, OwnerCoordinator};
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

pub(crate) fn attach(runtime: OrbitRuntime) -> OrbitRuntime {
    let runtime = attach_drain_owner(runtime);
    if runtime.worker_invocation().is_none() {
        return runtime;
    }
    let transport = Arc::new(WorkerOwner {
        runtime: runtime.clone(),
    });
    runtime.with_owner_coordinator(transport)
}

struct WorkerOwner {
    runtime: OrbitRuntime,
}

impl OwnerCoordinator for WorkerOwner {
    fn call(
        &self,
        name: &str,
        mut input: Value,
        mut session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let binding = session
            .worker_invocation
            .clone()
            .ok_or_else(|| OrbitError::PolicyDenied("worker context required".into()))?;
        let local = self.runtime.automation_machine_identity().ok_or_else(|| {
            OrbitError::PolicyDenied("execution machine identity unavailable".into())
        })?;
        if binding.execution.machine_id != local {
            return Err(OrbitError::PolicyDenied(
                "execution machine binding mismatch".into(),
            ));
        }
        if local == binding.owner_machine_id {
            if let Some(object) = input.as_object_mut() {
                object.insert(
                    "workspace".into(),
                    Value::String(binding.owner_workspace_id.clone()),
                );
            }
            session.workspace = Some(binding.owner_workspace_id);
            return self
                .runtime
                .execute_owner_coordination(name, input, session);
        }
        // [ORB-14260] Inside a masked agent sandbox the SSH route can only
        // fail on the masked `~/.ssh`. The nested `orbit` hands the calls
        // the run's coordinator carries to it before they reach here; any
        // other owner read from this process has no route at all.
        #[cfg(unix)]
        if orbit_core::runtime::plugin::sandbox_mask::plugin_trees_masked(
            &self.runtime.global_root(),
        ) {
            return Err(orbit_core::adapter::command::owner_route_unavailable(
                name,
                "this sandboxed process has no owner route of its own; only the claimed-owner \
                 tools cross the coordinator",
            ));
        }
        let remotes = federated::load_destinations(&self.runtime.global_root())?;
        let destinations = federated::federated_membership(local, local, remotes);
        let probe = SshDestinationProbe::new(
            local.into(),
            federated::DEFAULT_PROBE_TIMEOUT,
            federated::DEFAULT_ROUTED_DELIVERY_TIMEOUT,
            session.orchestrator.clone(),
            orbit_mcp::McpSessionAuthority::Agent,
        );
        let owner = FederatedMcpHost::new(destinations, Arc::new(probe));
        owner.call(name, input, session)
    }
}

/// Give every registered host a route to the owners in its host file
/// [ORB-13625] [ORB-14448]. Nothing is contacted here: the file is read
/// when a pull drain actually calls, so a host that never pulls pays nothing
/// and one whose destinations change picks them up on the next call.
fn attach_drain_owner(runtime: OrbitRuntime) -> OrbitRuntime {
    let Some(machine_id) = runtime.automation_machine_identity().map(ToOwned::to_owned) else {
        return runtime;
    };
    let transport = Arc::new(FederatedDrainOwner {
        global_root: runtime.global_root(),
        machine_id,
    });
    runtime.with_drain_owner_transport(transport)
}

/// A follower drain's owner route: the federated mux over this host's
/// registered hosts, opened as `agent` — the authority the owner's
/// pull, bind and settle rows require, and no more.
struct FederatedDrainOwner {
    global_root: PathBuf,
    machine_id: String,
}

impl FederatedDrainOwner {
    fn host(&self) -> Result<FederatedMcpHost, OrbitError> {
        let remotes = federated::load_destinations(&self.global_root)?;
        let destinations =
            federated::federated_membership(&self.machine_id, &self.machine_id, remotes);
        let probe = SshDestinationProbe::new(
            self.machine_id.clone(),
            federated::DEFAULT_PROBE_TIMEOUT,
            federated::DEFAULT_ROUTED_DELIVERY_TIMEOUT,
            None,
            orbit_mcp::McpSessionAuthority::Agent,
        );
        Ok(FederatedMcpHost::new(destinations, Arc::new(probe)))
    }
}

impl DrainOwnerTransport for FederatedDrainOwner {
    fn call(&self, selector: &str, name: &str, mut input: Value) -> Result<Value, OrbitError> {
        if let Some(object) = input.as_object_mut() {
            object.insert("workspace".into(), Value::String(selector.to_string()));
        }
        self.host()?
            .call_internal_drain(name, input, ToolSessionContext::default())
    }

    fn show_task(&self, selector: &str, mut input: Value) -> Result<Value, OrbitError> {
        if let Some(object) = input.as_object_mut() {
            object.insert("workspace".into(), Value::String(selector.to_string()));
        }
        self.host()?
            .call_tool("orbit.task.show", input, ToolSessionContext::default())
    }

    fn worker_coordinator(&self) -> Arc<dyn OwnerCoordinator> {
        Arc::new(FederatedWorkerRoute {
            owner: FederatedDrainOwner {
                global_root: self.global_root.clone(),
                machine_id: self.machine_id.clone(),
            },
        })
    }
}

/// The bound worker's coordination route to a remote owner. The mux refuses
/// any call without a worker binding and any binding whose owner selector
/// differs from the call's, so this cannot become an unbound owner session.
struct FederatedWorkerRoute {
    owner: FederatedDrainOwner,
}

impl OwnerCoordinator for FederatedWorkerRoute {
    fn call(
        &self,
        name: &str,
        input: Value,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        OwnerCoordinator::call(&self.owner.host()?, name, input, session)
    }
}
