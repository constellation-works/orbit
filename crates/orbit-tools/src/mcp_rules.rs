//! Internal MCP rules declared alongside the owning builtin definitions.
//! The adapter consumes these facts; it never infers behavior from tool names.
//!
//! A builtin's id-routing classification is declared at its definition
//! site — the `id_route` field of its operation spec, or
//! `Tool::declared_id_route` for a hand-written tool — and resolved into
//! [`McpToolRules::id_route`] here.
//! There is no name table: `builtin_mcp_rules` looks the name up in the
//! routes the registered builtin definitions themselves declared.

use std::collections::HashMap;
use std::sync::OnceLock;

use orbit_types::tool::{
    McpIdRoute, McpIdRouteDeclaration, McpInputCondition, McpInputDecoration, McpPresentationRead,
    McpToolRules, McpWorkspaceEcho, ToolSchema,
};

use crate::ToolRegistry;

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

/// Builtin rules for name-based CLI routing, including CLI-only task operations.
/// Registered plugins receive defaults through the registry instead.
///
/// The id route comes from the named builtin's own definition, never from
/// a hand-kept name list.
pub fn builtin_mcp_rules(name: &str) -> McpToolRules {
    let id_route = declared_builtin_id_routes()
        .get(name)
        .cloned()
        .unwrap_or(McpIdRoute::Unspecified);
    builtin_mcp_rules_with_route(name, id_route)
}

/// Every registered builtin's declared id route, by tool name.
///
/// Built once from a registry of the builtins themselves, so a tool that
/// declares its route at its definition site is picked up with no second
/// place to update, and CLI-only operations (registered inactive or
/// without an MCP scope) are included alongside MCP-advertised ones.
fn declared_builtin_id_routes() -> &'static HashMap<String, McpIdRoute> {
    static DECLARED: OnceLock<HashMap<String, McpIdRoute>> = OnceLock::new();
    DECLARED.get_or_init(|| {
        let mut registry = ToolRegistry::new();
        registry.register_builtins();
        registry.declared_id_routes()
    })
}

fn builtin_mcp_rules_with_route(name: &str, id_route: McpIdRoute) -> McpToolRules {
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
        declared_workspace_selector: matches!(name, "orbit.task.show" | "orbit.attention"),
        workspace_optional: matches!(name, "orbit.notice.list" | "orbit.attention"),
        presentation_read,
        input_decoration,
        ..McpToolRules::default()
    }
}
