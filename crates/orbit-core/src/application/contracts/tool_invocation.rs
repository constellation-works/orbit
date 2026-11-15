//! Tool invocation the application reaches through a port the adapter
//! implements [ORB-22004]. Every call still runs the adapter's tool chokepoint,
//! which holds the audit trail, entry-point attribution and capability check, so
//! a caller gets the same record it always did.

use orbit_common::OrbitError;
use orbit_tools::ToolContext;
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

use crate::runtime::tool_exec::CapabilityEnforcement;

/// Tool execution for application use cases. The adapter implements it on
/// `OrbitRuntime` by delegating to its tool entry methods.
pub(crate) trait ToolInvocationPort {
    /// A CLI-entry call under the caller's session, as
    /// `execute_tool_command_with_session_context` runs it.
    fn run_tool_command_with_session(
        &self,
        name: &str,
        input: Value,
        agent_override: Option<String>,
        model_override: Option<String>,
        session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError>;

    /// A registered tool run with `tool_context`, under the default
    /// capability enforcement.
    fn run_tool_with_context(
        &self,
        name: &str,
        input: Value,
        tool_context: ToolContext,
    ) -> Result<Value, OrbitError>;

    /// [`Self::run_tool_with_context`] with the caller's capability enforcement.
    fn run_tool_with_enforcement(
        &self,
        name: &str,
        input: Value,
        tool_context: ToolContext,
        enforcement: CapabilityEnforcement,
    ) -> Result<Value, OrbitError>;
}
