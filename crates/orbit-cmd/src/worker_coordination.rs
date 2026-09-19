//! Compose a managed worker with the existing owner MCP transport.
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_mcp::federated::{self, FederatedMcpHost, SshDestinationProbe};
use orbit_tools::OwnerCoordinator;
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

pub(crate) fn attach(runtime: OrbitRuntime) -> OrbitRuntime {
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
        let remotes = federated::load_destinations(&federated::destinations_path(
            &self.runtime.global_root(),
        ))?;
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
