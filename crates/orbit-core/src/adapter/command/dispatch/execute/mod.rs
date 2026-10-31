//! Run one tool call inside the audit boundary: authorize, execute, and
//! persist its audit row before surfacing the result.

mod audit_store;
mod entry;
mod finalize;
mod global;
mod runtime;

pub(super) use audit_store::BrokeredAudit;
pub use audit_store::ToolDispatchOutcome;
pub use entry::{
    ToolEntryPoint, mark_tool_audit_recorded, take_tool_audit_recorded, tool_suggestion_context,
};
pub use global::execute_global_in_process_tool_dispatch;
pub(in crate::adapter::command) use global::execute_global_plugin_dispatch;
