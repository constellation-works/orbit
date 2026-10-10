//! Task reservation and file-lock operations.

mod audit;
mod commands;
mod index;

pub(crate) use audit::{emit_expired_reservation_events, emit_task_lock_release_event};
pub(crate) use commands::{
    EmptyTaskSurfacePolicy, list, parse_task_ids, release, reserve, reserve_with_index,
    workspace_orbit_dir, workspace_task_reservation_id,
};
pub(crate) use index::{
    TaskLockIndex, TaskLockOverlap, active_task_lock_holders, lock_context_files_for_task,
    lock_holder_index, merge_task_lock_conflicts, requested_task_files_indexed,
    task_lock_conflicts_indexed, task_lock_overlaps,
};

#[cfg(test)]
mod tests;
