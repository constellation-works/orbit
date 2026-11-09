//! Task-id routing consumes the routing metadata each builtin tool
//! declares at its own definition site, resolved by orbit-tools into
//! `McpToolRules::id_route`.

use orbit_types::tool::McpIdRoute;
use serde_json::Value;

/// Whether `name` is an id-routed task tool.
pub fn is_id_routed_tool(name: &str) -> bool {
    orbit_tools::builtin_mcp_rules(name).id_route == McpIdRoute::Task
}

/// The task id an id-routed call addresses when it carries no explicit
/// selector, else `None`.
///
/// A blank or null `workspace` names nothing and counts as absent, matching
/// the v1 server's own reading. A non-string `workspace` is left for the
/// destination to refuse rather than read as absent, so a malformed selector
/// never turns into a prefix route.
pub fn id_only_task_target<'a>(name: &str, input: &'a Value) -> Option<&'a str> {
    if !is_id_routed_tool(orbit_types::tool::tool_operation_name(name, input)) {
        return None;
    }
    match input.get("workspace") {
        None | Some(Value::Null) => {}
        Some(Value::String(selector)) if selector.trim().is_empty() => {}
        Some(_) => return None,
    }
    input
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
}
