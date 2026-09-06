//! Task commands and coordinated record writes.

mod add;
pub(crate) mod contention;
mod helpers;
mod lint;
mod listing;
mod params;
mod paths;
mod query;
mod records;
mod transitions;
mod update;

pub use contention::{LockContentionHotspot, LockContentionReport};
pub use lint::{TaskLintFinding, TaskLintReport, TaskLintSeverity};
pub(crate) use listing::query_task_store;
pub use listing::{TaskCandidates, TaskListFilter, TaskListQuery, TaskPage, TaskRow};
pub(crate) use params::TaskRecordUpdateParams;
pub use params::{TaskAddParams, TaskUpdateParams};

pub(crate) use helpers::{SYSTEM_ACTOR_LABEL, TaskAttributionInput, assemble_task_attribution};
pub(crate) use paths::{
    canonicalize_context_files_for_read, compute_task_add_warnings, context_workspace_root,
};
pub(crate) use transitions::{
    ensure_resolves_targets_are_workspace_local, ensure_task_has_execution_plan,
    in_progress_transition_requires_plan,
};

#[cfg(test)]
mod tests;
