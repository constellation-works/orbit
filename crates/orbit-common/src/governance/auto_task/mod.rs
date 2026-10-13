//! The auto-task operation registry.
//!
//! [`operations`] is the single declaration site for every auto-task verb.
//! CLI, MCP and runtime handler wiring all derive from it.

pub mod operations;

pub use operations::{
    AUTO_TASK, AUTO_TASK_OPERATIONS, AutoTaskOperation, AutoTaskVerb, DEDUPE_VALUES,
    TASK_COMPLEXITY_VALUES, TASK_PRIORITY_VALUES, TASK_STATUS_VALUES, TASK_TYPE_VALUES,
};
