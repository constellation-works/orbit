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
mod family;
mod invocation;
mod mcp_rules;
mod mcp_toolset;
pub use error::{McpToolsetError, WorkerBindingError};
pub use family::{
    TOOL_ACTION_FIELD, TOOL_ACTION_SEPARATOR, TOOL_FAMILIES, TOOL_MODE_FIELD, ToolFamily,
    ToolFamilyAction, is_tool_family_operation, tool_call_name, tool_family, tool_family_action,
    tool_family_call, tool_family_of_operation, tool_family_operation_call, tool_operation_name,
    tool_policy_operations,
};
pub use invocation::{
    AGENT_CONTRACT_TOOLS, CLAIMED_MODE_DENIED_TOOLS, CLAIMED_OWNER_TOOLS, WorkerInvocation,
    is_claimed_leaf_reachable_tool, is_claimed_owner_tool,
};
pub use mcp_rules::{
    McpIdRoute, McpIdRouteDeclaration, McpInputCondition, McpInputDecoration, McpPresentationRead,
    McpToolRules, McpWorkspaceEcho,
};
pub use mcp_toolset::{McpRole, McpToolset, McpToolsetSelection};

pub use definition::{
    ExecutionResult, McpCapability, McpToolAnnotations, McpToolDefinition, McpToolDefinitionError,
    McpToolScope, McpTransport, StoredTool, ToolParam, ToolSchema, ToolSessionContext,
    is_exact_canonical_tool_name, mcp_advertised_tool_name, validate_mcp_tool_definitions,
};

mod attention;
pub use attention::*;
