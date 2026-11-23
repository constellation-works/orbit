//! Internal MCP rules declared alongside the owning builtin definitions.
//! The adapter consumes these facts; it never infers behavior from tool names.
//!
//! A builtin's id-routing classification is declared at its definition
//! site — the `id_route` field of its operation spec, or
//! `Tool::declared_id_route` for a hand-written tool — and resolved into
//! [`McpToolRules::id_route`] here.

use orbit_types::tool::{
    McpIdRoute, McpIdRouteDeclaration, McpInputCondition, McpInputDecoration, McpPresentationRead,
    McpToolRules, McpWorkspaceEcho, ToolSchema,
};

/// Rules for a registered tool. Plugins keep the default transport contract.
pub(crate) fn mcp_rules(
    schema: &ToolSchema,
    declared_id_route: McpIdRouteDeclaration,
) -> McpToolRules {
    if !schema.builtin {
        return McpToolRules::default();
    }
    builtin_mcp_rules_with_route(&schema.name, declared_id_route.resolve())
}

pub(crate) fn builtin_mcp_rules_with_route(name: &str, id_route: McpIdRoute) -> McpToolRules {
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
        // `view` belongs to run list and show only.
        "orbit.task.list" | "orbit.run" => McpWorkspaceEcho::WhenAny(present(&["view"])),
        "orbit.task.add" | "orbit.task.update" => McpWorkspaceEcho::WhenAny(present(
            crate::task_guarded_selectors::GUARDED_SELECTOR_KEYS,
        )),
        // Each condition's field belongs to one action: `view` to list,
        // `expected_enabled` to update, `acknowledge_unconditional` to mint.
        "orbit.auto_task" => McpWorkspaceEcho::WhenAny(present(&[
            "view",
            "expected_enabled",
            "acknowledge_unconditional",
        ])),
        "orbit.pipeline.invoke" => {
            McpWorkspaceEcho::WhenAny(vec![McpInputCondition::True("default_input".into())])
        }
        // Auto carries its own action as `mode`. Ship's `mode` (`pr` or
        // `local`) is a different parameter and must not require a selector.
        "orbit.workflow" => McpWorkspaceEcho::WhenAny(vec![McpInputCondition::All(vec![
            McpInputCondition::Equals {
                key: "action".into(),
                value: "auto".into(),
            },
            McpInputCondition::Present("mode".into()),
        ])]),
        "orbit.routine.control" => McpWorkspaceEcho::Always,
        _ => McpWorkspaceEcho::Never,
    };
    let presentation_read = match name {
        "orbit.task.show" => Some(McpPresentationRead::Task),
        // Read with `action: show`.
        "orbit.run" => Some(McpPresentationRead::Run),
        _ => None,
    };
    let input_decoration = match name {
        "orbit.friction" => McpInputDecoration::Friction,
        "orbit.search" => McpInputDecoration::Search,
        _ => McpInputDecoration::None,
    };
    McpToolRules {
        workspace_echo,
        id_route,
        declared_workspace_selector: matches!(name, "orbit.task.show" | "orbit.attention"),
        workspace_optional: matches!(name, "orbit.notice.list" | "orbit.attention"),
        presentation_read,
        input_decoration,
        ..McpToolRules::default()
    }
}
