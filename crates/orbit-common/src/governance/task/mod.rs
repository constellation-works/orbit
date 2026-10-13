//! The task operation registry.
//!
//! [`operations`] is the single declaration site for every migrated task
//! verb. CLI, MCP and runtime handler wiring derive from it; the task verbs
//! not yet migrated stay hand-wired beside them.

pub mod operations;
mod write_operations;

pub use operations::{
    TASK, TASK_COMPLEXITY_VALUES, TASK_HOST_HELP, TASK_OPERATIONS, TASK_PRIORITY_VALUES,
    TASK_STATUS_VALUES, TASK_TYPE_VALUES, TaskOperation, TaskVerb,
};
pub use write_operations::TASK_CLI_OPTIONS_PARAM;
