//! Shared entry adapter for CLI, Web, tool-host, and engine-host tool calls.

use orbit_common::OrbitError;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::tool_exec::{CapabilityEnforcement, resolve_task_id_from_context};

impl OrbitRuntime {
    /// In-process owner transport. The accepting runtime still applies the
    /// worker session's transaction fence; only execution-host routing is removed.
    pub fn execute_owner_coordination(
        &self,
        name: &str,
        input: Value,
        mut session: orbit_types::tool::ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let binding = session
            .worker_invocation
            .as_ref()
            .ok_or_else(|| OrbitError::PolicyDenied("worker context required".into()))?;
        if self.automation_machine_identity() != Some(binding.owner_machine_id.as_str()) {
            return Err(OrbitError::PolicyDenied(
                "owner process identity mismatch".into(),
            ));
        }
        let owner = self.without_worker_routing();
        session.process_machine_id = self.automation_machine_identity().map(str::to_owned);
        session
            .effective_capabilities
            .remove(&orbit_types::tool::McpCapability::Operator);
        owner
            .execute_tool_command_dispatch_with_session_context(
                name,
                input,
                None,
                None,
                crate::adapter::command::ToolEntryPoint::Mcp,
                session,
            )
            .map(|outcome| outcome.value)
    }

    pub(crate) fn execute_worker_projection(
        &self,
        name: &str,
        input: &Value,
        session: &orbit_types::tool::ToolSessionContext,
    ) -> Result<Option<Value>, OrbitError> {
        super::tool_host::worker_tools::execute(
            self,
            session,
            if name == "orbit.task.update" {
                orbit_tools::OrbitBuiltinAction::TaskUpdate
            } else {
                orbit_tools::OrbitBuiltinAction::TaskShow
            },
            input,
            None,
        )
    }

    pub fn run_tool(&self, name: &str, input: Value) -> Result<Value, OrbitError> {
        self.run_tool_with_role(name, input, Role::Admin)
    }

    /// Run a registered tool for a human-operated in-process adapter.
    /// The trusted label lives in `ToolContext`, never in agent-controlled
    /// input, so the ordinary public tool path remains family-validated.
    pub fn run_tool_as_human(&self, name: &str, input: Value) -> Result<Value, OrbitError> {
        self.run_tool_with_context_and_role(
            name,
            input,
            Role::Admin,
            ToolContext {
                trusted_actor_label: Some("human".to_string()),
                ..Default::default()
            },
        )
    }

    pub(crate) fn run_tool_with_role(
        &self,
        name: &str,
        input: Value,
        role: Role,
    ) -> Result<Value, OrbitError> {
        self.run_tool_with_context_and_role(name, input, role, ToolContext::default())
    }

    pub(crate) fn run_tool_with_context_and_role(
        &self,
        name: &str,
        input: Value,
        role: Role,
        tool_context: ToolContext,
    ) -> Result<Value, OrbitError> {
        self.run_tool_with_context_and_role_and_capability(
            name,
            input,
            role,
            tool_context,
            CapabilityEnforcement::Enforce,
        )
    }

    pub(crate) fn run_tool_with_context_and_role_and_capability(
        &self,
        name: &str,
        input: Value,
        _role: Role,
        mut tool_context: ToolContext,
        capability_enforcement: CapabilityEnforcement,
    ) -> Result<Value, OrbitError> {
        self.bind_worker_session(&mut tool_context.session_context)?;
        if tool_context.orbit_host.is_none() {
            let task_id = resolve_task_id_from_context(self, &tool_context)?;
            tool_context.orbit_host = Some(super::tool_host::build_orbit_tool_host(
                self,
                task_id,
                None,
                tool_context.session_context.clone(),
            ));
        }
        self.execute_registered_tool(name, input, tool_context, capability_enforcement)
    }
}
