use orbit_common::governance::auto_task::AUTO_TASK;
use orbit_common::governance::drain::DRAIN;
use orbit_common::governance::friction::FRICTION;
use orbit_common::governance::task::TASK;
use orbit_common::governance::workspace_claim::WORKSPACE_CLAIM;
use orbit_types::tool::McpToolScope;

use super::{agent, command, operation, pipeline, search, task, workflow};
use crate::ToolRegistry;

pub fn register(registry: &mut ToolRegistry) {
    registry.register_mcp(
        super::domain_control::WorkflowAutoTool,
        McpToolScope::WorkspaceRequired,
    );
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
    registry.register_mcp(task::add::OrbitTaskAddTool, McpToolScope::WorkspaceRequired);
    // Attach and read are the two halves of one artifact surface: without a
    // read verb an agent can store a reference it can never inspect again.
    registry.register_mcp(
        task::artifact_get::OrbitTaskArtifactGetTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        task::artifact_put::OrbitTaskArtifactPutTool,
        McpToolScope::WorkspaceRequired,
    );
    // Destructive administration remains reachable through non-MCP surfaces.
    registry.register_inactive(task::delete::OrbitTaskDeleteTool);
    registry.register_inactive(task::locks::list::OrbitTaskLocksTool);
    registry.register_inactive(task::locks::reserve::OrbitTaskLocksReserveTool);
    registry.register_inactive(task::locks::release::OrbitTaskLocksReleaseTool);
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
    // Task rejection is a human/operator decision — CLI / dashboard only.
    registry.register_inactive(task::reject::OrbitTaskRejectTool);
    registry.register_mcp(
        task::reconcile_review::OrbitTaskReconcileReviewTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        task::review_reset::OrbitTaskReviewResetTool,
        McpToolScope::WorkspaceRequired,
    );
    // The migrated task reads (`list`, `eligible`, `show`, `lint`) come from
    // the shared operation registry; the other task tools are hand-wired.
    operation::register_noun(registry, &TASK);
    registry.register_mcp(
        task::update::OrbitTaskUpdateTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        pipeline::invoke::OrbitPipelineInvokeTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register(pipeline::wait::OrbitPipelineWaitTool);
    registry.register_mcp(search::OrbitSearchTool, McpToolScope::WorkspaceRequired);
    registry.register_mcp(
        workflow::OrbitWorkflowShipTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        workflow::OrbitWorkflowRunShowTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        workflow::OrbitWorkflowRunListTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        workflow::OrbitWorkflowRunResumeTool,
        McpToolScope::WorkspaceRequired,
    );
}
