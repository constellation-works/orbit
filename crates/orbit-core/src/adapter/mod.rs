//! Protocol adapters that translate tool and engine-host requests into Core operations.

pub mod command;
pub(crate) mod engine_host;
mod tool_execution;
pub(crate) mod tool_host;

pub use tool_host::HubCoordinationExecutor;
