use orbit_common::governance::auto_task::AUTO_TASK;
use orbit_common::governance::codebase::CODEBASE;
use orbit_common::governance::drain::DRAIN;
use orbit_common::governance::friction::FRICTION;
use orbit_common::governance::task::TASK;
use orbit_common::governance::workflow::WORKFLOW;
use orbit_common::governance::workspace_claim::WORKSPACE_CLAIM;
use orbit_types::tool::McpToolScope;

use super::{agent, command, operation, pipeline};
use crate::ToolRegistry;
use orbit_common::governance::search::SEARCH;

pub fn register(registry: &mut ToolRegistry) {
    registry.register_mcp(
        super::domain_control::RoutineControlTool,
        McpToolScope::WorkspaceRequired,
    );
    // Managed workers can ask the owning host to edit its live definitions.
    // The child receives no raw write grant for the registered checkout.
    // Auto-task schemas, MCP exposure and execution kinds come from the shared
    // operation registry, like friction's below.
    operation::register_noun(registry, &AUTO_TASK);
    // Deterministic owner/follower operations remain active for runtime and
    // operator diagnostics, but have no public MCP schemas. The internal
    // transport selects their existing guarded handlers explicitly.
    operation::register_noun(registry, &DRAIN);
    // Friction schemas, MCP exposure and execution kinds are declared once in
    // the shared operation registry and registered from there.
    operation::register_noun(registry, &FRICTION);
    // Workspace claims are coordination holds like task locks and remain off
    // the MCP surface.
    operation::register_noun(registry, &WORKSPACE_CLAIM);
    // Agent invocation is workspace-scoped: the admission is made against the
    // checkout that owns it, and Core is where that decision lives.
    registry.register_mcp(agent::OrbitAgentInvokeTool, McpToolScope::WorkspaceRequired);
    // Command execution is workspace-scoped; Core retains its domain and claim
    // validation.
    registry.register_mcp(
        command::OrbitCommandExecTool,
        McpToolScope::WorkspaceRequired,
    );
    // Every task tool comes from the shared operation registry, including
    // the operator-only review verbs and the inactive reject and delete.
    operation::register_noun(registry, &TASK);
    registry.register_mcp(
        pipeline::invoke::OrbitPipelineInvokeTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register(pipeline::wait::OrbitPipelineWaitTool);
    operation::register_noun(registry, &SEARCH);
    // Read-only codebase analytics, the dashboard Codebase view's rankings.
    operation::register_noun(registry, &CODEBASE);
    // Workflow ship, run reads and resume, and the auto-drain control come
    // from the shared operation registry.
    operation::register_noun(registry, &WORKFLOW);
}
