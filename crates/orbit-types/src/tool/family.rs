//! Tool families: one agent-facing tool per operation family, selected by an
//! `action` field.
//!
//! A family such as `orbit.decision` is the tool MCP advertises, `orbit tool
//! run` accepts and activity policy names. Each action is served by one
//! operation (`orbit.decision.raise`), whose name is an internal identity: the
//! authorization catalog, the audit trail and host routing key on it, but no
//! caller can name it as a tool. A call names the family and an action.
//!
//! Policy lists (activity `tools`, `tool_disallow_list`, task `granted_tools`)
//! name a whole family (`orbit.decision`) or one action of it
//! (`orbit.decision:raise`). An operation name in a policy list matches
//! nothing.

use serde_json::Value;

/// The input field that selects a family action.
pub const TOOL_ACTION_FIELD: &str = "action";

/// The input field a family exposes for an operation's own `action` input,
/// which the family's selector would otherwise shadow.
pub const TOOL_MODE_FIELD: &str = "mode";

/// The separator between a family and an action in a policy entry.
pub const TOOL_ACTION_SEPARATOR: char = ':';

/// One agent-facing tool that groups operations behind an `action` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolFamily {
    /// Canonical tool name, such as `orbit.decision`.
    pub name: &'static str,
    /// The actions, in advertised order.
    pub actions: &'static [ToolFamilyAction],
}

/// One action of a [`ToolFamily`] and the operation that serves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolFamilyAction {
    /// The `action` value, such as `raise`.
    pub action: &'static str,
    /// The internal operation name, such as `orbit.decision.raise`.
    pub operation: &'static str,
    /// Whether the operation takes its own `action` input, which the family
    /// carries as [`TOOL_MODE_FIELD`].
    pub takes_mode: bool,
}

const fn action(action: &'static str, operation: &'static str) -> ToolFamilyAction {
    ToolFamilyAction {
        action,
        operation,
        takes_mode: false,
    }
}

const fn action_with_mode(action: &'static str, operation: &'static str) -> ToolFamilyAction {
    ToolFamilyAction {
        action,
        operation,
        takes_mode: true,
    }
}

/// Every tool family, by canonical name.
pub const TOOL_FAMILIES: &[ToolFamily] = &[
    ToolFamily {
        name: "orbit.task.recover",
        actions: &[
            action("recover_claim", "orbit.task.recover_claim"),
            action("requeue", "orbit.task.requeue"),
            action("repilot", "orbit.task.repilot"),
            action("retry_handoff", "orbit.task.retry_handoff"),
            action("revoke_handoff", "orbit.task.revoke_handoff"),
            action("review_reset", "orbit.task.review_reset"),
            action_with_mode("reconcile_review", "orbit.task.reconcile_review"),
        ],
    },
    ToolFamily {
        name: "orbit.run",
        actions: &[
            action("list", "orbit.workflow.run.list"),
            action("show", "orbit.workflow.run.show"),
            action("observe", "orbit.workflow.run.observe"),
            action("resume", "orbit.workflow.run.resume"),
        ],
    },
    ToolFamily {
        name: "orbit.workflow",
        actions: &[
            action_with_mode("auto", "orbit.workflow.auto"),
            action("ship", "orbit.workflow.ship"),
            action("land", "orbit.workflow.land"),
        ],
    },
    ToolFamily {
        name: "orbit.auto_task",
        actions: &[
            action("list", "orbit.auto_task.list"),
            action("add", "orbit.auto_task.add"),
            action("update", "orbit.auto_task.update"),
            action("mint", "orbit.auto_task.mint"),
        ],
    },
    ToolFamily {
        name: "orbit.decision",
        actions: &[
            action("list", "orbit.decision.list"),
            action("show", "orbit.decision.show"),
            action("raise", "orbit.decision.raise"),
        ],
    },
    ToolFamily {
        name: "orbit.codebase",
        actions: &[
            action("show", "orbit.codebase.show"),
            action("related", "orbit.codebase.related"),
            action("analytics", "orbit.codebase.analytics"),
        ],
    },
    ToolFamily {
        name: "orbit.friction",
        actions: &[
            action("add", "orbit.friction.add"),
            action("update", "orbit.friction.update"),
        ],
    },
    ToolFamily {
        name: "orbit.task.artifact",
        actions: &[
            action("get", "orbit.task.artifact.get"),
            action("put", "orbit.task.artifact.put"),
        ],
    },
];

impl ToolFamily {
    /// The action named `action`, if this family has it.
    pub fn action(&self, action: &str) -> Option<&'static ToolFamilyAction> {
        self.actions.iter().find(|entry| entry.action == action)
    }

    /// The action names, comma separated, for messages.
    pub fn action_names(&self) -> String {
        self.actions
            .iter()
            .map(|entry| entry.action)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The policy entry naming one action, such as `orbit.decision:raise`.
    pub fn qualified_action(&self, action: &str) -> String {
        format!("{}{TOOL_ACTION_SEPARATOR}{action}", self.name)
    }
}

/// The family named `name`.
pub fn tool_family(name: &str) -> Option<&'static ToolFamily> {
    TOOL_FAMILIES.iter().find(|family| family.name == name)
}

/// The family and action whose operation is `operation`.
pub fn tool_family_of_operation(
    operation: &str,
) -> Option<(&'static ToolFamily, &'static ToolFamilyAction)> {
    TOOL_FAMILIES.iter().find_map(|family| {
        family
            .actions
            .iter()
            .find(|entry| entry.operation == operation)
            .map(|entry| (family, entry))
    })
}

/// A task requirement or grant as a tool list names it. A name stored before
/// tool families [ORB-23280] (a minted task, a workspace's auto-task
/// definition) names the operation behind a family action, such as
/// `orbit.task.artifact.put`; it reads as that family action,
/// `orbit.task.artifact:put`. Any other name is returned unchanged.
pub fn tool_requirement_name(name: &str) -> std::borrow::Cow<'_, str> {
    match tool_family_of_operation(name) {
        Some((family, action)) => std::borrow::Cow::Owned(family.qualified_action(action.action)),
        None => std::borrow::Cow::Borrowed(name),
    }
}

/// Whether `name` is an internal family operation rather than a tool.
pub fn is_tool_family_operation(name: &str) -> bool {
    tool_family_of_operation(name).is_some()
}

/// The family and action a policy entry or operation names: `family:action`
/// or an operation name. A bare family or any other tool is `None`.
pub fn tool_family_action(name: &str) -> Option<(&'static ToolFamily, &'static ToolFamilyAction)> {
    if let Some((family, action)) = name.split_once(TOOL_ACTION_SEPARATOR) {
        let family = tool_family(family)?;
        return family.action(action).map(|action| (family, action));
    }
    tool_family_of_operation(name)
}

/// The operation a call to `name` with `input` reaches: a family call with a
/// known action names its operation, and anything else names itself. Name-keyed
/// rules (authorization, routing, audit classification) read this.
pub fn tool_operation_name<'a>(name: &'a str, input: &Value) -> &'a str {
    tool_family(name)
        .and_then(|family| {
            input
                .get(TOOL_ACTION_FIELD)
                .and_then(Value::as_str)
                .and_then(|action| family.action(action.trim()))
        })
        .map_or(name, |action| action.operation)
}

/// How a message names a call to `name`: `family:action` for a family
/// operation, which no caller can name, and `name` itself otherwise.
pub fn tool_call_name(name: &str) -> std::borrow::Cow<'_, str> {
    match tool_family_of_operation(name) {
        Some((family, action)) => std::borrow::Cow::Owned(family.qualified_action(action.action)),
        None => std::borrow::Cow::Borrowed(name),
    }
}

/// The operations a policy name covers: every action's for a bare family,
/// one for `family:action` or an operation name, else the name itself.
pub fn tool_policy_operations(name: &str) -> Vec<&str> {
    if let Some(family) = tool_family(name) {
        return family.actions.iter().map(|entry| entry.operation).collect();
    }
    match tool_family_action(name) {
        Some((_, action)) => vec![action.operation],
        None => vec![name],
    }
}

/// Re-address an operation's canonical input as a call to its family, for a
/// hop that hands the call to another process. Input to any other tool is
/// returned unchanged.
pub fn tool_family_call(operation: &str, mut input: Value) -> (String, Value) {
    let Some((family, action)) = tool_family_of_operation(operation) else {
        return (operation.to_string(), input);
    };
    if let Some(object) = input.as_object_mut() {
        if action.takes_mode
            && let Some(mode) = object.remove(TOOL_ACTION_FIELD)
        {
            object.insert(TOOL_MODE_FIELD.to_string(), mode);
        }
        object.insert(
            TOOL_ACTION_FIELD.to_string(),
            Value::String(action.action.to_string()),
        );
    }
    (family.name.to_string(), input)
}

/// The inverse of [`tool_family_call`]: a family call with a known action as
/// its operation's canonical input, for a hop that keys on operations before
/// the call reaches the registry. Any other call is returned unchanged.
/// Required fields are not checked here; the registry checks them when the
/// call is resolved for execution.
pub fn tool_family_operation_call(name: &str, mut input: Value) -> (String, Value) {
    let operation = tool_operation_name(name, &input);
    let Some((_, action)) = tool_family_of_operation(operation).filter(|_| operation != name)
    else {
        return (name.to_string(), input);
    };
    let operation = operation.to_string();
    if let Some(object) = input.as_object_mut() {
        object.remove(TOOL_ACTION_FIELD);
        if action.takes_mode
            && let Some(mode) = object.remove(TOOL_MODE_FIELD)
        {
            object.insert(TOOL_ACTION_FIELD.to_string(), mode);
        }
    }
    (operation, input)
}
