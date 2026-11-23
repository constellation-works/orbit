//! Task-id routing consumes the routing metadata each builtin tool
//! declares at its own definition site, resolved by orbit-tools into
//! `McpToolRules::id_route`.

use orbit_common::OrbitError;
use orbit_common::governance::task::{
    TASK_ID_WIRE_ALIAS, TASK_OPERATIONS, normalize_task_id_alias, task_id_alias_str,
};
use orbit_types::tool::{McpIdRoute, tool_operation_name};
use serde_json::Value;

/// Whether `name` is an id-routed task tool.
pub fn is_id_routed_tool(name: &str) -> bool {
    orbit_tools::builtin_mcp_rules(name).id_route == McpIdRoute::Task
}

/// Whether `operation` accepts `task_id` as an unadvertised wire alias for `id`.
fn accepts_task_id_alias(operation: &str) -> bool {
    is_id_routed_tool(operation)
        || TASK_OPERATIONS.iter().any(|op| {
            op.tool_name == operation
                && op
                    .wire_params()
                    .any(|param| param.name == "id" && param.aliases.contains(&TASK_ID_WIRE_ALIAS))
        })
}

/// Canonicalize `task_id` to `id` before routing or dispatching to a
/// destination, so an owner serving the preceding `id`-only wire contract
/// receives `id`, while a conflicting (`id` + `task_id`) pair fails before
/// dispatch.
pub(super) fn normalize_task_id_alias_if_accepted(
    name: &str,
    input: &mut Value,
) -> Result<(), OrbitError> {
    if !accepts_task_id_alias(tool_operation_name(name, input)) {
        return Ok(());
    }
    if let Some(object) = input.as_object_mut() {
        normalize_task_id_alias(object)?;
    }
    Ok(())
}

/// The task id an id-routed call addresses when it carries no explicit
/// selector, else `None`.
///
/// A blank or null `workspace` names nothing and counts as absent, matching
/// the v1 server's own reading. A non-string `workspace` is left for the
/// destination to refuse rather than read as absent, so a malformed selector
/// never turns into a prefix route.
///
/// Accepts `task_id` when `id` is absent (`orbit_common::governance::task::TASK_ID_WIRE_ALIAS`
/// — ORB-26989 exception to `#8734`); returns `None` when both are present.
pub fn id_only_task_target<'a>(name: &str, input: &'a Value) -> Option<&'a str> {
    if !is_id_routed_tool(tool_operation_name(name, input)) {
        return None;
    }
    match input.get("workspace") {
        None | Some(Value::Null) => {}
        Some(Value::String(selector)) if selector.trim().is_empty() => {}
        Some(_) => return None,
    }
    task_id_alias_str(input)
}
