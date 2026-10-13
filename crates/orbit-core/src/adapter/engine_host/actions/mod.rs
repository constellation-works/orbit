//! Deterministic actions and executor helpers behind `RuntimeHost`.
//!
//! `dispatch` maps each `CoreDeterministicAction` the engine runs onto Core:
//! sweep filing and task-pilot actions call `application::sweep_filing` and
//! `application::task::pilot`; the admission, drain, pull, pipeline and
//! recovery actions are still implemented in the sibling modules here. The
//! executor helpers resolve what an agent step runs with: the CLI command
//! (`cli_executor`), the sandbox (`sandbox`) and the task context
//! (`task_context`). HTTP agent-loop transport and CLI subprocess execution
//! live in `orbit-engine`, so this module never names orbit-agent types.

pub(super) mod admission;
pub(super) mod blocked_recovery;
pub(super) mod child_dispatch;
pub(super) mod cli_executor;
pub(super) mod dispatch;
pub(super) mod finding_verification;
pub(super) mod pipeline_actions;
pub(crate) mod pull;
pub(super) mod sandbox;
pub(super) mod task_context;
#[cfg(test)]
mod tests;
pub(super) mod workspace_auto;
