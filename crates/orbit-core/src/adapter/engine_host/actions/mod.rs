//! Deterministic actions and executor helpers behind `RuntimeHost`.
//!
//! `dispatch` maps each `CoreDeterministicAction` the engine runs onto Core:
//! sweep filing, task-pilot, finding verification, blocked-task recovery,
//! backlog admission, workspace auto-drain, pull drain and pipeline actions
//! call `application::sweep_filing`, `application::task::pilot`,
//! `application::task::finding_verification_steps`,
//! `application::task::blocked_recovery::steps`, `application::admission`,
//! `application::workspace_auto`, `application::pull_drain` and
//! `application::job::pipeline_actions`. What stays here is engine transport:
//! `pipeline_actions` dispatches child Jobs and waits on them through
//! `child_dispatch`, and `pull` keeps the pull drain's owner transport and
//! leaf launcher adapters. The executor helpers resolve what an agent step runs with:
//! the CLI command (`cli_executor`), the sandbox (`sandbox`) and the task
//! context (`task_context`). HTTP agent-loop transport and CLI subprocess execution
//! live in `orbit-engine`, so this module never names orbit-agent types.

pub(super) mod child_dispatch;
pub(super) mod cli_executor;
pub(super) mod dispatch;
pub(super) mod pipeline_actions;
pub(crate) mod pull;
pub(super) mod sandbox;
pub(super) mod task_context;
