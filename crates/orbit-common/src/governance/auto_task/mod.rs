//! The auto-task operation registry.
//!
//! [`operations`] is the single declaration site for every auto-task verb.
//! CLI, MCP and runtime handler wiring all derive from it.

pub mod operations;

pub use operations::{
    AUTO_TASK, AUTO_TASK_OPERATIONS, AutoTaskOperation, AutoTaskVerb, DEDUPE_VALUES,
};
