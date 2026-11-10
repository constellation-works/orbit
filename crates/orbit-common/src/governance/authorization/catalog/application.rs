use orbit_types::tool::McpCapability;

use super::{GOVERNED_OPERATIONS, GovernedOperation, TASK_GRANT_TOOLS_OPERATION_ID, governed_tool};

/// One row of the closed built-in application-consent ceiling. A consent may
/// narrow a row, never exceed it; a built-in without a row is refused. Every
/// admitted call still passes the ordinary domain lifecycle checks, and no
/// row grants a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationOperationCeiling {
    /// Canonical built-in tool name.
    pub tool: &'static str,
    /// Keys an exact (`input`, schema v1) operation may carry, entry or
    /// callback. Empty means the exact form is refused.
    pub exact_fields: &'static [&'static str],
    /// Whether a constrained (`fields`) entry operation may name this tool.
    pub constrained_entry: bool,
    /// Whether a constrained completion callback may name this tool.
    pub constrained_callback: bool,
    /// Fields a constrained operation may list.
    pub constrained_fields: &'static [&'static str],
}

/// The closed built-in application-consent ceiling table. A consent may
/// narrow a row and may not exceed it. `orbit.auto_task.add` and
/// `orbit.auto_task.update` are callback-only: the exact form is refused,
/// and every add field except `workspace` must be `artifact_json`.
pub const APPLICATION_OPERATION_CEILINGS: &[ApplicationOperationCeiling] = &[
    ApplicationOperationCeiling {
        tool: "orbit.task.update",
        exact_fields: &["id", "status", "comment", "workspace"],
        constrained_entry: true,
        constrained_callback: true,
        constrained_fields: &[
            "id",
            "status",
            "comment",
            "note",
            "plan",
            "execution_summary",
            "workspace",
        ],
    },
    ApplicationOperationCeiling {
        tool: "orbit.decision.answer",
        exact_fields: &["id", "answer", "note", "workspace"],
        constrained_entry: false,
        constrained_callback: false,
        constrained_fields: &[],
    },
    ApplicationOperationCeiling {
        tool: "orbit.task.show",
        exact_fields: &[],
        constrained_entry: false,
        constrained_callback: true,
        constrained_fields: &["id", "fields", "workspace"],
    },
    ApplicationOperationCeiling {
        tool: "orbit.task.artifact.get",
        exact_fields: &[],
        constrained_entry: false,
        constrained_callback: true,
        constrained_fields: &["id", "path", "workspace"],
    },
    ApplicationOperationCeiling {
        tool: "orbit.auto_task.add",
        exact_fields: &[],
        constrained_entry: false,
        constrained_callback: true,
        constrained_fields: &[
            "name",
            "description",
            "schedule",
            "template",
            "dedupe",
            "workspace",
        ],
    },
    ApplicationOperationCeiling {
        tool: "orbit.auto_task.update",
        exact_fields: &[],
        constrained_entry: false,
        constrained_callback: true,
        constrained_fields: &["name", "enabled", "workspace"],
    },
];

/// Statuses an exact `orbit.task.update` operation may set.
pub const APPLICATION_EXACT_TASK_STATUSES: &[&str] = &["backlog", "done", "rejected", "archived"];

/// Statuses a constrained `orbit.task.update` may name as `const` or
/// `one_of`. Every other target needs a `transition` constraint.
pub const APPLICATION_DIRECT_TASK_STATUSES: &[&str] = &["done", "rejected", "archived"];

/// Statuses a constrained `orbit.task.update` `transition` may target.
pub const APPLICATION_TRANSITION_TASK_STATUSES: &[&str] = &[
    "in-progress",
    "review",
    "done",
    "rejected",
    "archived",
    "backlog",
];

/// The only statuses a `transition` to `backlog` may start from. `backlog`
/// from `proposed`, `someday` or `blocked` approves work for dispatch, so it
/// is never application authority.
pub const APPLICATION_BACKLOG_TRANSITION_SOURCES: &[&str] = &["review", "in-progress"];

/// Fields `orbit.task.show` may return to a constrained callback.
pub const APPLICATION_TASK_SHOW_FIELDS: &[&str] = &[
    "id",
    "status",
    "tags",
    "terminal",
    "artifacts",
    "crew",
    "updated_at",
    "execution_summary",
];

/// Input-dependent floors: a tool input carrying `field` additionally
/// requires the governed operation `operation`, beside the tool's own row.
/// The tool chokepoint applies each one; application consent reads them to
/// refuse such inputs outright.
pub const INPUT_DEPENDENT_OPERATION_FLOORS: &[(&str, &str, &str)] = &[
    (
        "orbit.pipeline.invoke",
        "default_input",
        "orbit.routine.control",
    ),
    (
        "orbit.auto_task.update",
        "expected_enabled",
        "orbit.routine.control",
    ),
    (
        "orbit.auto_task.mint",
        "acknowledge_unconditional",
        "orbit.routine.control",
    ),
    (
        "orbit.task.add",
        "granted_tools",
        TASK_GRANT_TOOLS_OPERATION_ID,
    ),
    (
        "orbit.task.update",
        "granted_tools",
        TASK_GRANT_TOOLS_OPERATION_ID,
    ),
    (
        "orbit.auto_task.add",
        "granted_tools",
        TASK_GRANT_TOOLS_OPERATION_ID,
    ),
    (
        "orbit.auto_task.update",
        "granted_tools",
        TASK_GRANT_TOOLS_OPERATION_ID,
    ),
    ("orbit.task.update", "repilot", "orbit.task.repilot"),
    ("orbit.auto_task.list", "view", "orbit.workflow.run.show"),
];

/// The ceiling row for a built-in tool, or `None` when application consent
/// can never name it.
pub fn application_operation_ceiling(tool: &str) -> Option<&'static ApplicationOperationCeiling> {
    APPLICATION_OPERATION_CEILINGS
        .iter()
        .find(|ceiling| ceiling.tool == tool)
}

/// Whether a consent may list `tool` as a completion callback: the ceiling
/// table's callback column, exact or constrained. An application-bound plugin
/// session sends every such tool to the host, where the consent witness is.
pub fn application_callback_tool(tool: &str) -> bool {
    application_operation_ceiling(tool)
        .is_some_and(|ceiling| ceiling.constrained_callback || !ceiling.exact_fields.is_empty())
}

/// Whether a built-in tool, or one of the input fields it would be called
/// with, carries an Operator floor in the governed-operation catalogue. Such
/// an operation is never application authority, whatever a consent says.
/// Derived from the catalogue rows, so a new operator-only row is covered
/// without editing a list here.
pub fn application_operator_floored<'a>(
    tool: &str,
    mut fields: impl Iterator<Item = &'a str>,
) -> bool {
    let operator_floor =
        |operation: &GovernedOperation| operation.allowed.contains(&McpCapability::Operator);
    if governed_tool(tool).is_some_and(operator_floor) {
        return true;
    }
    fields.any(|field| {
        INPUT_DEPENDENT_OPERATION_FLOORS
            .iter()
            .filter(|(floored, input, _)| *floored == tool && *input == field)
            .any(|(_, _, operation)| {
                GOVERNED_OPERATIONS
                    .iter()
                    .any(|row| row.id == *operation && operator_floor(row))
            })
    })
}
