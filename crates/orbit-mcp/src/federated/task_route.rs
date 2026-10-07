//! Task-id routing: which tools address one task by id, and when a call does.
//!
//! A task id's prefix names the host that writes it, so a call that addresses
//! one task by id and names no workspace goes to that host. The set of such
//! tools is a static list rather than a schema inference: a tool joins it by
//! review, and the surface test refuses a tool with a required `id` and an
//! optional workspace that is neither listed here nor excluded with a reason.

use serde_json::Value;

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
];

/// Tools that take a required `id` and an optional workspace but are not
/// id-routed, each with the reason.
pub const NOT_ID_ROUTED_TOOLS: &[(&str, &str)] = &[
    (
        "orbit.task.lint",
        "lints a task against the selected workspace's configuration; it keeps selector behavior",
    ),
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
        "orbit.workflow.run.show",
        "a run id is host-local: a run executes where it was admitted",
    ),
    (
        "orbit.workflow.run.resume",
        "a run id is host-local: a run executes where it was admitted",
    ),
];

/// Whether `name` is an id-routed task tool.
pub fn is_id_routed_tool(name: &str) -> bool {
    ID_ROUTED_TASK_TOOLS.contains(&name)
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
