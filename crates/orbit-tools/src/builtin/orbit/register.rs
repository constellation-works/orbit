use orbit_types::tool::McpToolScope;

use super::{
    agent, auto_task, command, drain, friction, pipeline, search, task, workflow, workspace_claim,
};
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
    registry.register_mcp(
        auto_task::add::OrbitAutoTaskAddTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        auto_task::list::OrbitAutoTaskListTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        auto_task::mint::OrbitAutoTaskMintTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_inactive(auto_task::show::OrbitAutoTaskShowTool);
    registry.register_mcp(
        auto_task::update::OrbitAutoTaskUpdateTool,
        McpToolScope::WorkspaceRequired,
    );
    // Deterministic owner/follower operations remain active for runtime and
    // operator diagnostics, but have no public MCP schemas. The internal
    // transport selects their existing guarded handlers explicitly.
    registry.register(drain::probe::OrbitDrainProbeTool);
    registry.register(drain::receipt_lookup::OrbitDrainReceiptLookupTool);
    registry.register(drain::pull::OrbitTaskPullTool);
    registry.register(drain::claim_bind::OrbitDrainClaimBindTool);
    registry.register(drain::claim_settle::OrbitDrainClaimSettleTool);
    registry.register(drain::claims::OrbitDrainClaimsTool);
    // Friction schemas and MCP exposure are declared once in the shared
    // operation registry and registered from there.
    friction::register(registry);
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
    registry.register_inactive(task::lint::OrbitTaskLintTool);
    registry.register_inactive(task::locks::list::OrbitTaskLocksTool);
    registry.register_inactive(task::locks::reserve::OrbitTaskLocksReserveTool);
    registry.register_inactive(task::locks::release::OrbitTaskLocksReleaseTool);
    // Workspace claims are coordination holds like task locks and remain off
    // the MCP surface.
    registry.register_inactive(workspace_claim::OrbitWorkspaceClaimAcquireTool);
    registry.register_inactive(workspace_claim::OrbitWorkspaceClaimReleaseTool);
    registry.register_inactive(workspace_claim::OrbitWorkspaceClaimShowTool);
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
        task::review_reset::OrbitTaskReviewResetTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        task::show::OrbitTaskShowTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        task::list::OrbitTaskListTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        task::eligible::OrbitTaskEligibleTool,
        McpToolScope::WorkspaceRequired,
    );
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
