//! Internal MCP rules declared alongside the owning builtin definitions.
//! The adapter consumes these facts; it never infers behavior from tool names.

use orbit_types::tool::{
    McpIdRoute, McpInputCondition, McpInputDecoration, McpPresentationRead, McpToolRules,
    McpWorkspaceEcho, ToolSchema,
};

/// Tools whose target is exactly one task id and whose `workspace` is
/// optional. Called without a selector, they route by the id's prefix.
pub const ID_ROUTED_TASK_TOOLS: &[&str] = &[
    "orbit.task.show",
    "orbit.task.update",
    "orbit.task.reject",
    "orbit.task.delete",
    "orbit.task.artifact.get",
    "orbit.task.artifact.put",
    "orbit.task.review_reset",
    "orbit.task.reconcile_review",
    "orbit.task.requeue",
];

/// Tools that take a required `id` and an optional workspace but are not
/// id-routed, each with the reason.
pub const NOT_ID_ROUTED_TOOLS: &[(&str, &str)] = &[
    (
        "orbit.friction.show",
        "a friction id is workspace-local and carries no host prefix",
    ),
    (
        "orbit.friction.update",
        "a friction id is workspace-local and carries no host prefix",
    ),
    (
        "orbit.friction.resolve",
        "a friction id is workspace-local and carries no host prefix",
    ),
    (
        "orbit.friction.rehome",
        "a friction id is workspace-local and carries no host prefix",
    ),
    (
        "orbit.decision.show",
        "a decision request id is workspace-local and carries no host prefix",
    ),
    (
        "orbit.decision.answer",
        "a decision request id is workspace-local and carries no host prefix",
    ),
    (
        "orbit.workflow.run.show",
        "a run id is host-local: a run executes where it was admitted",
    ),
    (
        "orbit.workflow.run.resume",
        "a run id is host-local: a run executes where it was admitted",
    ),
];

/// Rules for a registered tool. Plugins keep the default transport contract.
pub(crate) fn mcp_rules(schema: &ToolSchema) -> McpToolRules {
    if !schema.builtin {
        return McpToolRules::default();
    }
    builtin_mcp_rules(&schema.name)
}

/// Builtin rules for name-based CLI routing, including CLI-only task operations.
/// Registered plugins receive defaults through the registry instead.
pub fn builtin_mcp_rules(name: &str) -> McpToolRules {
    let id_route = if ID_ROUTED_TASK_TOOLS.contains(&name) {
        McpIdRoute::Task
    } else if let Some((_, reason)) = NOT_ID_ROUTED_TOOLS.iter().find(|(tool, _)| *tool == name) {
        McpIdRoute::WorkspaceLocal((*reason).into())
    } else {
        McpIdRoute::Unspecified
    };
    let present = |keys: &[&str]| {
        keys.iter()
            .map(|key| McpInputCondition::Present((*key).into()))
            .collect::<Vec<_>>()
    };
    let workspace_echo = match name {
        "orbit.task.show" => McpWorkspaceEcho::WhenAny(vec![
            McpInputCondition::Present("view".into()),
            McpInputCondition::True("snapshot".into()),
        ]),
        "orbit.task.list"
        | "orbit.workflow.run.show"
        | "orbit.workflow.run.list"
        | "orbit.auto_task.list" => McpWorkspaceEcho::WhenAny(present(&["view"])),
        "orbit.task.add" | "orbit.task.update" => McpWorkspaceEcho::WhenAny(present(&[
            "request_id",
            "expected_revision",
            "verdict",
            "complete",
        ])),
        "orbit.auto_task.update" => McpWorkspaceEcho::WhenAny(present(&["expected_enabled"])),
        "orbit.auto_task.mint" => {
            McpWorkspaceEcho::WhenAny(present(&["acknowledge_unconditional"]))
        }
        "orbit.pipeline.invoke" => {
            McpWorkspaceEcho::WhenAny(vec![McpInputCondition::True("default_input".into())])
        }
        "orbit.workflow.auto" | "orbit.routine.control" => McpWorkspaceEcho::Always,
        _ => McpWorkspaceEcho::Never,
    };
    let presentation_read = match name {
        "orbit.task.show" => Some(McpPresentationRead::Task),
        "orbit.workflow.run.show" => Some(McpPresentationRead::Run),
        _ => None,
    };
    let input_decoration = match name {
        "orbit.friction.add" | "orbit.friction.update" => McpInputDecoration::Friction,
        "orbit.search" => McpInputDecoration::Search,
        _ => McpInputDecoration::None,
    };
    McpToolRules {
        workspace_echo,
        id_route,
        declared_workspace_selector: name == "orbit.task.show",
        presentation_read,
        input_decoration,
    }
}
