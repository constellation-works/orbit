//! Orchestration for `backend: cli` agent subprocess dispatch.

mod completion;
mod dispatch;
mod policy;
mod prepare;

pub use dispatch::run_cli_backend;
pub(crate) use dispatch::run_cli_backend_for_step;
pub use policy::activity_tool_policy_env;
