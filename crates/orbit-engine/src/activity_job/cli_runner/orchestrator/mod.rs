//! Orchestration for `backend: cli` agent subprocess dispatch.

mod completion;
mod dispatch;
mod entry;
mod environment;
mod evidence;
mod grok_usage;
mod limit;
mod policy;
mod prepare;

#[cfg(test)]
mod tests;

pub(crate) use dispatch::run_cli_backend_for_step;
pub use entry::run_cli_backend;
pub(crate) use environment::provider_child_environment;
pub use environment::{CLAUDE_WORKER_CREDENTIAL_REMEDY, claude_worker_credential_held};
pub use policy::activity_tool_policy_env;
