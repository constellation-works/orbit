//! Domain contracts for this Orbit types module.

mod definition;
mod error;
mod invocation;
pub use error::ToolError;
pub use invocation::WorkerInvocation;

pub use definition::{
    ExecutionResult, McpCapability, McpToolDefinition, McpToolDefinitionError, McpToolScope,
    McpTransport, StoredTool, ToolParam, ToolSchema, ToolSessionContext,
    is_exact_canonical_tool_name, mcp_advertised_tool_name, validate_mcp_tool_definitions,
};
