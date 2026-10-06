//! A claimed worker's MCP owner calls go where its CLI calls go.
//!
//! A managed worker's MCP server runs inside the agent sandbox, which masks
//! the SSH credentials its owner route needs. Its owner coordination calls are
//! handed to the run's broker, or refused, exactly as `orbit tool run` hands
//! or refuses them; every other call reaches the wrapped host unchanged.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_core::adapter::command::{
    ClaimedOwnerRoute, ToolEntryPoint, bridge_claimed_owner_call,
    execute_global_in_process_tool_dispatch,
};
use orbit_mcp::McpHost;
use orbit_types::tool::{McpToolDefinition, ToolSessionContext};
use serde_json::Value;

/// Wraps a worker's MCP host with the claimed-owner route.
pub(super) struct ClaimedOwnerBridge {
    pub(super) inner: Arc<dyn McpHost>,
    pub(super) global_root: PathBuf,
    pub(super) process_machine_id: String,
}

impl McpHost for ClaimedOwnerBridge {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        self.inner.list_mcp_tool_definitions()
    }

    fn friction_tag_taxonomy(
        &self,
        session_context: &ToolSessionContext,
    ) -> Result<Option<Vec<(String, String)>>, OrbitError> {
        self.inner.friction_tag_taxonomy(session_context)
    }

    fn hidden_tool_names(&self, session_context: &ToolSessionContext) -> BTreeSet<String> {
        self.inner.hidden_tool_names(session_context)
    }

    fn call_tool(
        &self,
        name: &str,
        input: Value,
        session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let cwd = std::env::current_dir()?;
        if let Some(route) = bridge_claimed_owner_call(
            &self.global_root,
            session_context.worker_invocation.as_ref(),
            Some(&self.process_machine_id),
            name,
            &input,
            &cwd,
            &cwd,
            ToolEntryPoint::Mcp,
        ) {
            return match route {
                ClaimedOwnerRoute::Forwarded(result) => result,
                ClaimedOwnerRoute::Refused(error) => {
                    // The inner ServerMcpHost is deliberately bypassed for a
                    // bridged owner call. Persist adapter-side refusals and
                    // transport failures here, with this MCP session's caller
                    // and call identity; a broker response already owns its
                    // audit row and is returned above without a duplicate.
                    execute_global_in_process_tool_dispatch(
                        &self.global_root,
                        name,
                        input,
                        ToolEntryPoint::Mcp,
                        session_context,
                        move |_| Err(error),
                    )
                    .map(|outcome| outcome.value)
                }
            };
        }
        self.inner.call_tool(name, input, session_context)
    }

    fn call_internal_drain(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.inner.call_internal_drain(name, input, context)
    }

    fn refuse_internal_drain(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.inner.refuse_internal_drain(name, input, context)
    }

    fn federated_workspace_selectors(&self) -> bool {
        self.inner.federated_workspace_selectors()
    }
}
