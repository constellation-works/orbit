use orbit_types::tool::McpToolScope;

use super::{
    agent, auto_task, command, desktop, drain, friction, pipeline, search, task, workflow,
    workspace_claim,
};
use crate::ToolRegistry;

pub fn register(registry: &mut ToolRegistry) {
    // Shipped canonical routes remain callable for existing clients, without advertisements.
    for tool in [
        desktop::DesktopTool::Read,
        desktop::DesktopTool::Drain,
        desktop::DesktopTool::Automation,
        desktop::DesktopTool::Snapshot,
        desktop::DesktopTool::Write,
    ] {
        registry.register(tool);
    }
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
    registry.register_mcp(
        auto_task::toggle::OrbitAutoTaskToggleTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        auto_task::delete::OrbitAutoTaskDeleteTool,
        McpToolScope::WorkspaceRequired,
    );
    // The distributed drain. The probe, receipt lookup, pull, bind and settle
    // are advertised because a follower's drain reaches each of them over
    // federated MCP [ORB-13625]. Claim inspection is registered active but
    // unadvertised: it is an operator surface, and the operator reaches it
    // with `orbit tool run orbit.drain.claims` rather than through a dedicated
    // subcommand, so `register_inactive` would leave it with no entry point at
    // all [ORB-12581]. What keeps it operator-only is its `GOVERNED_OPERATIONS`
    // row, which refuses an agent on every surface. Approval, revocation and
    // recovery are deliberately absent: they are owner-operator dashboard
    // actions, not something an executor's session may reach.
    registry.register_mcp(
        drain::probe::OrbitDrainProbeTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        drain::receipt_lookup::OrbitDrainReceiptLookupTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        drain::pull::OrbitTaskPullTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        drain::claim_bind::OrbitDrainClaimBindTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        drain::claim_settle::OrbitDrainClaimSettleTool,
        McpToolScope::WorkspaceRequired,
    );
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
        task::show::OrbitTaskShowTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        task::list::OrbitTaskListTool,
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
    // [ORB-13744] The narrow public delivery read beside the operator-only
    // run view. Deliberately ungoverned, like `orbit.task.show`: it answers
    // only typed host evidence for a task of this workspace, and a plugin
    // still needs it in `permissions.orbit_tools`.
    registry.register_mcp(
        workflow::OrbitWorkflowRunDeliveryTool,
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
    registry.register_mcp(
        workflow::OrbitWorkflowRunWorkersTool,
        McpToolScope::WorkspaceRequired,
    );
}
