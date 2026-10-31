//! The result policy of `orbit.task.list`, decided once below the transports.
//!
//! Without a `fields` projection the list handler builds full task records. A
//! default call answers with compact [`MINIMAL_TASK_FIELDS`] rows instead, so
//! `orbit tool run` and MCP return one default shape for one tool name. A call
//! whose input already projects rows (`fields`, its `field` alias, or a
//! bounded `view`) keeps the handler's answer. Transports pick a
//! [`TaskListResultProfile`] from their own flags and render what
//! [`TaskListResultPolicy::finalize`] returns; they keep no copy of this rule.
//! Callers that need full records (the noun CLI, HTTP) never apply it.

use orbit_common::governance::task::{MINIMAL_TASK_FIELDS, project_task_list_output};
use serde_json::Value;

const TASK_LIST_TOOL: &str = "orbit.task.list";

/// The output shape a transport asks for, parsed from its own flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskListResultProfile<'a> {
    /// The tool's default result: compact rows unless the input projects them.
    Default,
    /// The operation's complete output (CLI `--full`).
    Full,
    /// Per-task fields the caller selects from the output (CLI `--fields`).
    /// An empty list selects nothing and is the [`Self::Default`] profile.
    Fields(&'a [String]),
}

/// How one tool call's result is finalized, decided from its name, input and
/// profile before the call so the input can move into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskListResultPolicy(ListShape);

#[derive(Debug, Clone, PartialEq, Eq)]
enum ListShape {
    /// Not an `orbit.task.list` call; this policy leaves its result alone.
    NotTaskList,
    /// Return the operation's output as is.
    Unchanged,
    /// Keep the default [`MINIMAL_TASK_FIELDS`] in each row.
    Compact,
    /// Keep these fields in each row.
    Fields(Vec<String>),
}

impl TaskListResultPolicy {
    /// The policy for calling `tool_name` with `input` under `profile`.
    pub fn for_call(tool_name: &str, input: &Value, profile: TaskListResultProfile<'_>) -> Self {
        if tool_name != TASK_LIST_TOOL {
            return Self(ListShape::NotTaskList);
        }
        Self(match profile {
            TaskListResultProfile::Full => ListShape::Unchanged,
            TaskListResultProfile::Fields(fields) if !fields.is_empty() => {
                ListShape::Fields(fields.to_vec())
            }
            TaskListResultProfile::Default | TaskListResultProfile::Fields(_) => {
                if input_projects_rows(input) {
                    ListShape::Unchanged
                } else {
                    ListShape::Compact
                }
            }
        })
    }

    /// Whether this policy owns the call's result shape. A transport renders
    /// an owned result as [`Self::finalize`] returns it, with no shaping of
    /// its own.
    pub fn owns_result(&self) -> bool {
        self.0 != ListShape::NotTaskList
    }

    /// The call's result after this policy. Rows are projected inside the
    /// `tasks` array; `total`, `truncated`, warnings and every other envelope
    /// key are kept. Applying the policy to an already finalized result
    /// returns it unchanged.
    pub fn finalize(&self, output: Value) -> Value {
        match &self.0 {
            ListShape::NotTaskList | ListShape::Unchanged => output,
            ListShape::Compact => project_task_list_output(output, MINIMAL_TASK_FIELDS),
            ListShape::Fields(fields) => project_task_list_output(output, fields),
        }
    }
}

/// `fields`, its `field` alias, or `view` present and non-null means the
/// handler already shaped each row, so the default compact rows must not
/// silently discard the long fields it was asked for.
fn input_projects_rows(input: &Value) -> bool {
    ["fields", "field", "view"]
        .iter()
        .any(|key| !input.get(key).is_none_or(Value::is_null))
}
