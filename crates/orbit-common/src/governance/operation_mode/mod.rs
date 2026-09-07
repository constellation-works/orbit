//! The operation-mode operation registry [ORB-11332].
//!
//! [`operations`] is the single declaration site for every `orbit operation`
//! verb. CLI, MCP, dashboard, and runtime handler wiring all derive from it.

pub mod operations;

pub use operations::{
    OPERATION_MODE_OPERATIONS, OperationModeOperation, OperationModeVerb, operation_mode_operation,
};

#[cfg(test)]
mod tests;
