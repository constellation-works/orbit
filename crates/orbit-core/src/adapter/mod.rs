//! Protocol adapters that translate tool and engine-host requests into Core operations.

mod attention_wait;
pub mod command;
pub(crate) mod engine_host;
mod tool_execution;
pub(crate) mod tool_host;

pub use tool_host::HubCoordinationExecutor;
