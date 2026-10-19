//! Locked, retried, idempotent persistence of one validated task-pilot assessment.

mod apply_task;
mod atomic;
mod drift;
mod history;
/// A test seam at the two points of an application's lock protocol, so a test
/// can order concurrent applications deterministically.
#[cfg(test)]
pub(super) mod lock_hook;
mod reports;

pub(super) use apply_task::{ApplyTaskOutcome, apply_task, operation_was_applied};
pub(super) use atomic::{record_applied_assessment, task_operation_id, with_task_locks};
pub(super) use drift::assessed_material_drift;
pub(super) use history::no_target_assessment_marker;
pub(super) use reports::{
    failed_partition, source_superseded, stale_task, superseded_task, task_outcome,
};
