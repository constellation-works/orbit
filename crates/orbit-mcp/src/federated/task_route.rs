//! Task-id routing consumes the canonical tool definitions' routing metadata.
//! The classification lists remain exported for callers of the existing API;
//! their sole declaration lives with the builtin rules in orbit-tools.

use orbit_types::tool::McpIdRoute;
use serde_json::Value;

pub use orbit_tools::{ID_ROUTED_TASK_TOOLS, NOT_ID_ROUTED_TOOLS};

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
    if !is_id_routed_tool(name) {
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
