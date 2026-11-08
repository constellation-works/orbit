//! The one table placing each builtin MCP tool in its `orbit mcp serve`
//! toolset. Roles bundle these toolsets (`orbit_types::tool::McpRole`), so
//! moving a tool here moves it in every role that serves its toolset.
//!
//! A builtin MCP tool missing from this table belongs to no toolset: the
//! registry then refuses to build MCP definitions, so a new tool cannot ship
//! unplaced. Tools no MCP server advertises are in [`McpToolset::Local`] and
//! plugin tools in [`McpToolset::Plugins`]; neither is listed here.

use orbit_types::tool::McpToolset;

/// The toolset of one builtin MCP-advertised tool, by canonical name.
pub(crate) fn builtin_mcp_toolset(name: &str) -> Option<McpToolset> {
    use McpToolset as T;
    let toolset = match name {
        "orbit.attention"
        | "orbit.task.show"
        | "orbit.task.list"
        | "orbit.task.eligible"
        | "orbit.task.artifact.get"
        | "orbit.task.block_triage" => T::TasksRead,
        "orbit.task.add"
        | "orbit.task.update"
        | "orbit.task.artifact.put"
        | "orbit.friction.add"
        | "orbit.friction.update" => T::TasksWrite,
        "orbit.task.reconcile_review"
        | "orbit.task.recover_claim"
        | "orbit.task.repilot"
        | "orbit.task.requeue"
        | "orbit.task.retry_handoff"
        | "orbit.task.review_reset"
        | "orbit.task.revoke_handoff" => T::TaskRepair,
        "orbit.decision.list" | "orbit.decision.show" | "orbit.decision.raise" => T::Decisions,
        // Only the bound channel plugin or the operator calls these.
        "orbit.decision.digest" | "orbit.decision.submit" => T::DecisionChannel,
        "orbit.workflow.run.list" | "orbit.workflow.run.show" | "orbit.workflow.run.observe" => {
            T::Runs
        }
        "orbit.workflow.ship"
        | "orbit.workflow.land"
        | "orbit.workflow.auto"
        | "orbit.workflow.run.resume"
        | "orbit.pipeline.invoke" => T::Dispatch,
        "orbit.auto_task.add"
        | "orbit.auto_task.list"
        | "orbit.auto_task.mint"
        | "orbit.auto_task.update"
        | "orbit.automation.explain"
        | "orbit.routine.control" => T::Automation,
        "orbit.codebase.show" | "orbit.codebase.related" | "orbit.codebase.analytics" => {
            T::Codebase
        }
        "orbit.search" => T::Search,
        "orbit.command.exec" => T::Exec,
        _ if name.starts_with("orbit.notice.") => T::Notice,
        _ if name.starts_with("orbit.resident.") || name.starts_with("orbit.evolution.") => {
            T::Resident
        }
        // Crew memory is the memory experiment arm's surface.
        _ if name.starts_with("orbit.experiment.") || name.starts_with("orbit.memory.") => {
            T::Experiments
        }
        _ if name.starts_with("orbit.agents.")
            || name.starts_with("orbit.crew.")
            || name.starts_with("orbit.orchestrator.") =>
        {
            T::Analytics
        }
        _ => return None,
    };
    Some(toolset)
}
