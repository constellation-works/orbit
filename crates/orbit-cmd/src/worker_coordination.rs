//! Compose a runtime with the existing owner MCP transport: a managed
//! worker's coordination route, and a follower drain's route to its owner.

use std::path::PathBuf;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_core::app::tool::{self, ToolEntryPoint};
use orbit_core::app::workspace::Opening;
use orbit_mcp::federated::{
    self, CompositeDestinationProbe, FederatedMcpHost, InProcessDestinationProbe,
    SshDestinationProbe,
};
use orbit_mcp::{McpHost, McpSessionAuthority};
use orbit_tools::{DrainOwnerTransport, OwnerCoordinator};
use orbit_types::tool::{McpToolDefinition, ToolSessionContext, tool_operation_name};
use serde_json::Value;

struct WorkerOwner {
    runtime: Opening,
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
        if orbit_runtime::plugin::sandbox_mask::plugin_trees_masked(&self.runtime.global_root()) {
            return Err(orbit_core::app::tool::owner_route_unavailable(
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

/// A drain's owner route: the federated mux over this host's registered
/// hosts, opened as `agent` — the authority the owner's pull, bind and
/// settle rows require, and no more. Local owner selectors route in-process
/// against this host's registered checkout instead of opening SSH to this
/// machine [ORB-29594].
struct FederatedDrainOwner {
    global_root: PathBuf,
    machine_id: String,
}

impl FederatedDrainOwner {
    fn host(&self) -> Result<FederatedMcpHost, OrbitError> {
        let identity = orbit_mcp::mcp_server_identity(
            &self.global_root,
            None,
            McpSessionAuthority::Agent,
            &orbit_common::Env::default(),
        )?;
        let remotes = federated::load_destinations(&self.global_root)?;
        let destinations = federated::federated_membership(
            &self.machine_id,
            &identity.process_machine_name,
            remotes,
        );
        let local_host = Arc::new(LocalDrainMcpHost {
            global_root: self.global_root.clone(),
            process_machine_id: identity.process_machine_id,
            process_machine_name: identity.process_machine_name,
        });
        let probe = CompositeDestinationProbe::new(
            Arc::new(InProcessDestinationProbe::new(
                local_host,
                identity.session_context,
            )),
            Arc::new(SshDestinationProbe::new(
                self.machine_id.clone(),
                federated::DEFAULT_PROBE_TIMEOUT,
                federated::DEFAULT_ROUTED_DELIVERY_TIMEOUT,
                None,
                McpSessionAuthority::Agent,
            )),
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

    fn task_prefix_host(&self, prefix: &str) -> Result<Option<String>, OrbitError> {
        Ok(
            orbit_registry::hosts::load_task_prefix_table(&self.global_root)?
                .host_for_prefix(prefix)
                .map(|entry| entry.machine_id.clone()),
        )
    }

    fn show_task_by_id(&self, input: Value) -> Result<Value, OrbitError> {
        crate::hosts::routed_client(
            &self.global_root,
            &self.machine_id,
            orbit_mcp::McpSessionAuthority::Agent,
        )?
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

/// In-process local owner surface for [`FederatedDrainOwner`], enforcing the
/// same workspace-registry resolution, capability-class gate and local
/// `agent`-scoped session context as the federated MCP server.
struct LocalDrainMcpHost {
    global_root: PathBuf,
    process_machine_id: String,
    process_machine_name: String,
}

impl LocalDrainMcpHost {
    fn list_federated_workspaces(&self) -> Result<Value, OrbitError> {
        let registry_path =
            orbit_registry::workspace_registry::registry_path_for(&self.global_root);
        let registry = orbit_registry::workspace_registry::load_validated_registry_from(
            &registry_path,
            orbit_registry::workspace_registry::WorkspaceValidationMode::InMemory,
        )?;
        let host_facts =
            crate::hosts::local_host_facts(&self.global_root, &self.process_machine_id);
        Ok(orbit_mcp::execute_federated_workspace_discovery(
            &registry,
            &host_facts,
        ))
    }

    fn call_workspace_tool(
        &self,
        name: &str,
        mut input: Value,
        mut context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let selector = input
            .get("workspace")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|selector| !selector.is_empty())
            .or_else(|| {
                context
                    .workspace
                    .as_deref()
                    .map(str::trim)
                    .filter(|selector| !selector.is_empty())
            })
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "tool '{name}' requires an explicit workspace selector"
                ))
            })?;
        let selected =
            crate::registry_runtime::RegisteredRuntimeFactory::resolve_workspace_selector(
                &self.global_root,
                selector,
            )?;
        let operation = tool_operation_name(name, &input);
        federated::ensure_tool_class_held(
            operation,
            federated::CapabilityClasses::for_checkout(&selected.workspace, &selected.checkout),
        )?;
        // Clear any worker binding inherited from a calling claimed leaf
        // process (such as leaf self-settlement) so the owner endpoint
        // executes against its own store rather than re-entering worker
        // routing.
        let core = crate::registry_runtime::RegisteredRuntimeFactory::open_registered_checkout(
            &self.global_root,
            &selected.workspace,
            &selected.checkout,
        )?
        .without_worker_routing()
        .into_command_core();
        let repo_root = selected.checkout.repo_root.to_string_lossy().into_owned();
        context.workspace_id = Some(selected.workspace.id);
        context.workspace = Some(repo_root.clone());
        context.process_machine_id = Some(self.process_machine_id.clone());
        context.process_machine_name = Some(self.process_machine_name.clone());
        if let Some(object) = input.as_object_mut()
            && object.contains_key("workspace")
        {
            object.insert("workspace".to_string(), Value::String(repo_root));
        }
        tool::exec(
            &core,
            tool::Command::Run {
                name: name.to_string(),
                input,
                agent: None,
                model: None,
                entry_point: ToolEntryPoint::Mcp,
                session: context,
            },
        )?
        .into_value()
    }
}

impl McpHost for LocalDrainMcpHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        orbit_mcp::canonical_mcp_tool_definitions()
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))
    }

    fn refuse_internal_drain(
        &self,
        _name: &str,
        _input: Value,
        _context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        Err(OrbitError::PolicyDenied(
            "distributed drain requires the internal runtime route".into(),
        ))
    }

    fn call_internal_drain(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let Some(canonical) = orbit_mcp::internal_drain_name(name) else {
            return self.refuse_internal_drain(name, input, context);
        };
        self.call_workspace_tool(canonical, input, context)
    }

    fn call_tool(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        if let Some(canonical) = orbit_mcp::internal_drain_name(name) {
            return self.refuse_internal_drain(canonical, input, context);
        }
        if name == orbit_mcp::FEDERATED_DESTINATION_WORKSPACE_LIST_TOOL {
            return self.list_federated_workspaces();
        }
        self.call_workspace_tool(name, input, context)
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

pub(crate) fn attach(runtime: Opening) -> Opening {
    let runtime = attach_drain_owner(runtime);
    if !runtime.is_worker() {
        return runtime;
    }
    let transport = Arc::new(WorkerOwner {
        runtime: runtime.clone(),
    });
    runtime.with_owner_coordinator(transport)
}

/// Give every registered host a route to the owners in its host file
/// [ORB-13625] [ORB-14448]. Nothing is contacted here: the file is read
/// when a pull drain actually calls, so a host that never pulls pays nothing
/// and one whose destinations change picks them up on the next call.
fn attach_drain_owner(runtime: Opening) -> Opening {
    let Some(machine_id) = runtime.automation_machine_identity().map(ToOwned::to_owned) else {
        return runtime;
    };
    let transport = Arc::new(FederatedDrainOwner {
        global_root: runtime.global_root(),
        machine_id,
    });
    runtime.with_drain_owner_transport(transport)
}
