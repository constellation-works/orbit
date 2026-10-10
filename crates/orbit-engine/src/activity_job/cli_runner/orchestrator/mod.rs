//! Orchestration for `backend: cli` agent subprocess dispatch.

mod completion;
mod dispatch;
mod limit;
mod policy;
mod prepare;

#[cfg(test)]
mod tests;

pub use dispatch::run_cli_backend;
pub(crate) use dispatch::{provider_child_environment, run_cli_backend_for_step};
pub use policy::activity_tool_policy_env;
