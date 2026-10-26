//! Operator-granted tools: the task field naming tools an operator authorized
//! the implementing activity to use despite its deny list.
//!
//! Unlike `required_tools`, which any filer may set, a grant is written only
//! under operator authority and only while the task has no live attempt. This
//! module holds the field's name, its history event and the note that event
//! records; authorization and the change window live in `orbit-core`.

/// The task field that names the operator-granted tools.
pub const GRANTED_TOOLS_FIELD: &str = "granted_tools";

/// History event written for every change to a task's `granted_tools`.
pub const GRANTED_TOOLS_CHANGED_EVENT: &str = "granted_tools_changed";

/// The note a [`GRANTED_TOOLS_CHANGED_EVENT`] records: the tool sets before
/// and after the change and the task's run/claim state when it was made.
/// The writer's identity is the history entry's `by`.
#[must_use]
pub fn granted_tools_change_note(before: &[String], after: &[String], state: &str) -> String {
    format!(
        "granted_tools changed from {} to {}; {state}",
        tool_set(before),
        tool_set(after)
    )
}

fn tool_set(tools: &[String]) -> String {
    if tools.is_empty() {
        "none".to_string()
    } else {
        tools
            .iter()
            .map(|tool| format!("`{tool}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}
