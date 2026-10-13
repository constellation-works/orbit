//! MCP host trait dispatch and call-time workspace selectors.

use std::collections::BTreeSet;
use std::sync::Arc;

use orbit_cmd::task_owner::WorkspaceIdentity;
use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_mcp::McpHost;
use orbit_types::tool::{McpToolDefinition, McpToolScope, ToolSessionContext};
use serde_json::Value;

use super::host::ServerMcpHost;

impl McpHost for ServerMcpHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        self.advertised_definitions()
            .map(|definitions| definitions.as_ref().clone())
    }

    fn dynamic_mcp_tool_definitions(
        &self,
    ) -> Result<Option<Arc<Vec<McpToolDefinition>>>, OrbitError> {
        self.advertised_definitions().map(Some)
    }

    fn friction_tag_taxonomy(
        &self,
        context: &ToolSessionContext,
    ) -> Result<Option<Vec<(String, String)>>, OrbitError> {
        if context.workspace.is_none() {
            return Ok(None);
        }
        let input = Value::Object(Default::default());
        // Schema decoration is advisory: a session hint that does not resolve
        // (an unregistered runtime identity, a foreign path) advertises the
        // shipped defaults here and fails closed at call time, so tools/list
        // stays available for globally resolved tools such as task.show.
        let Ok((runtime, _selected)) =
            self.resolve_workspace_runtime("orbit.friction.add", &input, context)
        else {
            return Ok(None);
        };
        runtime.friction_tag_taxonomy().map(Some)
    }

    /// A workspace-scoped plugin tool is left out of `tools/list` where its
    /// plugin is switched off: in the bound workspace for a bound session, or
    /// in every registered workspace for an unbound one. `mcp_scope: global`
    /// tools follow the host state alone and are never hidden here. Any read
    /// that fails hides nothing — `tools/call` refuses on its own.
    fn hidden_tool_names(&self, context: &ToolSessionContext) -> BTreeSet<String> {
        let owners = match self.plugin_surface.resolve(&self.global_root) {
            Ok(surface) if !surface.workspace_tool_owners.is_empty() => {
                surface.workspace_tool_owners.clone()
            }
            _ => return BTreeSet::new(),
        };
        let switched_off: BTreeSet<String> = if context.workspace.is_some() {
            let Some(probe) = owners.keys().next() else {
                return BTreeSet::new();
            };
            let input = Value::Object(Default::default());
            let Ok((runtime, _)) = self.resolve_workspace_runtime(probe, &input, context) else {
                return BTreeSet::new();
            };
            owners
                .values()
                .filter(|plugin| runtime.ensure_plugin_enabled_in_workspace(plugin).is_err())
                .cloned()
                .collect()
        } else {
            self.plugins_off_in_every_workspace(owners.values())
        };
        owners
            .into_iter()
            .filter(|(_, plugin)| switched_off.contains(plugin))
            .map(|(tool, _)| tool)
            .collect()
    }

    fn refuse_internal_drain(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.audit_global_failure(
            name,
            input,
            context,
            OrbitError::PolicyDenied(
                "distributed drain requires the internal runtime route".into(),
            ),
        )
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
        // The mux's destination-side discovery path is intentionally absent
        // from tools/list. It retains Invalid local checkouts for descriptor
        // health without changing direct v1 orbit.workspace.list behavior.
        if name == orbit_mcp::FEDERATED_DESTINATION_WORKSPACE_LIST_TOOL {
            return self.call_global_tool(name, input, context);
        }
        let definition = match self.definition(name) {
            Ok(definition) => definition,
            Err(error) => return self.audit_global_failure(name, input, context, error),
        };
        if definition.scope == McpToolScope::Global {
            if self.is_plugin_tool(name) {
                return self.call_global_plugin_tool(name, input, context);
            }
            return self.call_global_tool(name, input, context);
        }
        self.call_workspace_tool(name, input, context)
    }
}

/// The selector the call itself passed, trimmed. Distinguishing "the caller
/// named a workspace" from "the session announced one" is what makes an
/// explicit selector a filter and the ambient one a default.
///
/// A blank selector names nothing, so it is absent (the session's binding
/// applies) rather than an override that then fails as if none were bound. A
/// selector of any other type is refused: reading it as absent would send the
/// call to the session's workspace instead of the one the caller meant.
pub(super) fn call_workspace_selector(input: &Value) -> Result<Option<&str>, OrbitError> {
    match input.get("workspace") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(selector)) => Ok(Some(selector.trim()).filter(|s| !s.is_empty())),
        Some(_) => Err(OrbitError::InvalidInput(
            "`workspace` must be a string: a registered workspace name, a logical workspace ID (`ws_*`), or an absolute checkout path"
                .to_string(),
        )),
    }
}

pub(super) fn execute_core_tool(
    runtime: &OrbitRuntime,
    name: &str,
    input: Value,
    context: ToolSessionContext,
    owner: Option<&WorkspaceIdentity>,
) -> Result<Value, OrbitError> {
    let output = runtime
        .execute_tool_command_dispatch_with_session_context(
            name,
            input.clone(),
            None,
            None,
            ToolEntryPoint::Mcp,
            context,
        )?
        .value;
    crate::command::task::support::attach_bound_workspace_identity(name, &input, owner, output)
}
