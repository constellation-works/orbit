//! What a run reaches its drain owner through: a claimed worker's routed tool
//! calls ([`ClaimOwner`]) and a follower drain's owner transport.
//!
//! A routed call is a whole tool call, not a task read or write: it carries a
//! session, is checked as host-owned or not, and reaches notices, search,
//! friction and artifacts as well as tasks. So it has its own path here and
//! does not go through the task access in `stored`, which reads a task record
//! or refuses a local write of one.

use std::path::Path;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_types::tool::{McpCapability, ToolSessionContext, WorkerInvocation};
use serde_json::Value;

use super::Run;
use crate::RuntimeKernel;
use crate::stored::task_access::{OwnerRoute, worker_binding_missing};

/// A claimed worker's channel to the drain owner that holds its claim.
#[derive(Clone, Copy)]
pub struct ClaimOwner<'a> {
    route: &'a OwnerRoute,
    /// This worker's repository root, the one local name a call's
    /// `workspace` input may carry.
    checkout: &'a Path,
}

impl<'a> ClaimOwner<'a> {
    /// The claim every call on this channel is bound to.
    pub fn binding(&self) -> &'a WorkerInvocation {
        self.route.invocation()
    }

    /// Whether the owner is another machine, so nothing on this one is the
    /// owner's checkout or store.
    pub fn is_remote(&self) -> bool {
        let binding = self.binding();
        binding.execution.machine_id != binding.owner_machine_id
    }

    /// Send one tool call to the owner under the caller's session.
    pub fn call(
        &self,
        name: &str,
        input: Value,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        check_routed_call(name, &input, &session)?;
        self.route.call(self.checkout, name, input, session)
    }

    /// Send one tool call the host itself makes. The host owns internal
    /// projections. Ordinary tool dispatch cannot select this route by adding
    /// a key to its input or changing its env.
    pub fn host_call(&self, name: &str, input: Value) -> Result<Value, OrbitError> {
        self.call(
            name,
            input,
            ToolSessionContext {
                worker_host_call: true,
                ..Default::default()
            },
        )
    }
}

impl Run {
    /// The drain owner this process's claim is held by. Present exactly for a
    /// claimed worker.
    pub fn claim_owner(&self) -> Option<ClaimOwner<'_>> {
        let route = self.stored().owner_route()?;
        Some(ClaimOwner {
            route,
            checkout: &self.base().paths().repo_root,
        })
    }
}

impl RuntimeKernel {
    /// Install the transport a follower drain reaches its owner through
    /// [ORB-13625]. Composition supplies it; nothing in a payload can.
    pub fn with_drain_owner_transport(
        mut self,
        transport: Arc<dyn orbit_tools::DrainOwnerTransport>,
    ) -> Self {
        self.run_mut().drain_owner_transport = Some(transport);
        self
    }

    pub fn drain_owner_transport(&self) -> Option<&Arc<dyn orbit_tools::DrainOwnerTransport>> {
        self.run().drain_owner_transport.as_ref()
    }

    /// Read a host-owned projection that is not a task read from the owner,
    /// with the owner route's lock-busy retry and response decoding. Task
    /// reads go through [`crate::Stored::task`].
    pub fn read_owner_tool_request<T: serde::de::DeserializeOwned>(
        &self,
        tool: &str,
        request: Value,
    ) -> Result<T, OrbitError> {
        check_search_scope(tool, &request)?;
        let owner = self.bound_claim_owner()?;
        owner.route.read(owner.checkout, tool, request)
    }

    pub fn bind_worker_session(&self, session: &mut ToolSessionContext) -> Result<(), OrbitError> {
        if let Some(owner) = self.stored().owner_route() {
            owner.bind_session(session)?;
        }
        if session.worker_invocation.is_some() {
            session
                .effective_capabilities
                .remove(&McpCapability::Operator);
        }
        Ok(())
    }

    /// [`ClaimOwner::call`], refused when this process holds no claim.
    pub fn route_worker_tool(
        &self,
        name: &str,
        input: Value,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        // The input is checked before the claim, so a refused input reads the
        // same from any process.
        check_routed_call(name, &input, &session)?;
        let owner = self.bound_claim_owner()?;
        owner.route.call(owner.checkout, name, input, session)
    }

    /// [`ClaimOwner::host_call`], refused when this process holds no claim.
    pub fn route_worker_host_tool(&self, name: &str, input: Value) -> Result<Value, OrbitError> {
        self.route_worker_tool(
            name,
            input,
            ToolSessionContext {
                worker_host_call: true,
                ..Default::default()
            },
        )
    }

    fn bound_claim_owner(&self) -> Result<ClaimOwner<'_>, OrbitError> {
        self.run().claim_owner().ok_or_else(worker_binding_missing)
    }
}

/// What every routed call is checked for before it leaves this process.
fn check_routed_call(
    name: &str,
    input: &Value,
    session: &ToolSessionContext,
) -> Result<(), OrbitError> {
    check_worker_host_input(input, session.worker_host_call)?;
    check_search_scope(name, input)
}

fn check_search_scope(tool: &str, request: &Value) -> Result<(), OrbitError> {
    if tool == "orbit.search" {
        check_worker_search_scope(request)?;
    }
    Ok(())
}

pub fn check_worker_host_input(input: &Value, host_call: bool) -> Result<(), OrbitError> {
    if !host_call
        && ["_worker_read", "_worker_update", "_worker_steering"]
            .iter()
            .any(|key| input.get(key).is_some())
    {
        return Err(OrbitError::PolicyDenied(
            "internal worker projections require a host-owned channel".into(),
        ));
    }
    Ok(())
}

/// Search from a claimed worker may read only its bound owner workspace.
/// Check both canonical fields and aliases even if both are supplied: parser
/// precedence must never turn a conflicting alias into a scope escape.
pub fn check_worker_search_scope(input: &Value) -> Result<(), OrbitError> {
    let wider = ["workspaces", "workspace_scope"].iter().any(|key| {
        input.get(key).is_some_and(|value| {
            !value.is_null()
                && !value.as_array().is_some_and(Vec::is_empty)
                && !value.as_str().is_some_and(|s| s.trim().is_empty())
        })
    }) || ["all_workspaces", "allWorkspaces"].iter().any(|key| {
        input.get(key).is_some_and(|value| {
            !value.is_null()
                && value.as_bool() != Some(false)
                && !value
                    .as_str()
                    .is_some_and(|s| s.trim().eq_ignore_ascii_case("false"))
        })
    });
    if wider {
        return Err(OrbitError::PolicyDenied(
            "claimed_owner_bridge_refused: search is limited to the claimed workspace; federated scope is unavailable".into(),
        ));
    }
    Ok(())
}

pub fn is_coordination_tool(name: &str) -> bool {
    orbit_types::tool::tool_policy_operations(name)
        .into_iter()
        .any(is_coordination_operation)
}

fn is_coordination_operation(name: &str) -> bool {
    name.starts_with("orbit.task.")
        || name.starts_with("orbit.friction.")
        || matches!(
            name,
            "orbit.search"
                | "orbit.auto_task.add"
                | "orbit.auto_task.update"
                | "orbit.notice.post"
                | "orbit.notice.show"
                | "orbit.notice.close"
        )
}
