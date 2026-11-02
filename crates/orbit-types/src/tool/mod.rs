//! Domain contracts for this Orbit types module.

mod application;
mod definition;
pub use application::{
    APPLICATION_CONSENT_SCHEMA_V1, APPLICATION_CONSENT_SCHEMA_V2, ApplicationAdmissionMatch,
    ApplicationArtifactDigest, ApplicationArtifactEvidence, ApplicationArtifactJson,
    ApplicationAuditRefusal, ApplicationConsent, ApplicationConsentRequest, ApplicationConstraint,
    ApplicationEmptyConstraint, ApplicationInvocation, ApplicationOperation,
    ApplicationPluginConsent, ApplicationPredicateEvidence, ApplicationRateLimit, ApplicationScope,
    ApplicationTaskPredicate, ApplicationTextConstraint, ApplicationTextDigest,
    ApplicationTokenConstraint, ApplicationTransition,
};
mod error;
mod invocation;
mod mcp_rules;
mod mcp_toolset;
pub use error::{McpToolsetError, WorkerBindingError};
pub use invocation::{
    AGENT_CONTRACT_TOOLS, CLAIMED_MODE_DENIED_TOOLS, CLAIMED_OWNER_TOOLS, WorkerInvocation,
    is_claimed_owner_tool,
};
pub use mcp_rules::{
    McpIdRoute, McpInputCondition, McpInputDecoration, McpPresentationRead, McpToolRules,
    McpWorkspaceEcho,
};
pub use mcp_toolset::{McpToolset, McpToolsetSelection};

pub use definition::{
    ExecutionResult, McpCapability, McpToolAnnotations, McpToolDefinition, McpToolDefinitionError,
    McpToolScope, McpTransport, StoredTool, ToolParam, ToolSchema, ToolSessionContext,
    is_exact_canonical_tool_name, mcp_advertised_tool_name, validate_mcp_tool_definitions,
};
