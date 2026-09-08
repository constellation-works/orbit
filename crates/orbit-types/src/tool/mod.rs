//! Domain contracts for this Orbit types module.

mod definition;
mod error;
pub use error::ToolError;

pub use definition::{
    CallerIdentityProof, ExecutionResult, McpCapability, McpToolDefinition, McpToolDefinitionError,
    McpToolScope, McpTransport, RemoteAgentInvokeMode, RemoteCallerGrant, StoredTool, ToolParam,
    ToolSchema, ToolSessionContext, is_exact_canonical_tool_name, mcp_advertised_tool_name,
    validate_mcp_tool_definitions,
};
