//! The task operation registry.
//!
//! [`operations`] is the single declaration site for every task tool. CLI,
//! MCP and runtime handler wiring derive from it; the task subcommands with
//! no tool stay hand-wired beside them.

mod artifact_hints;
mod lifecycle_operations;
mod nested_operations;
pub mod operations;
mod write_operations;

pub use artifact_hints::{
    ARTIFACT_LISTING_HINT_CLI, ARTIFACT_LISTING_HINT_MCP, artifact_listing_hint,
};
pub use operations::{
    TASK, TASK_COMPLEXITY_VALUES, TASK_HOST_HELP, TASK_OPERATIONS, TASK_ORIGIN_VALUES,
    TASK_PRIORITY_VALUES, TASK_STATUS_VALUES, TASK_TYPE_VALUES, TaskOperation, TaskVerb,
};
pub use write_operations::TASK_CLI_OPTIONS_PARAM;
