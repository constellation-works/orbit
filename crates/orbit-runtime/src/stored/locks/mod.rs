//! Task reservation, file-lock and path-lease operations.

mod audit;
mod command;
mod index;
pub mod lease;
mod prepared;
mod projection;

pub use crate::base::config::unlocked::UnlockedContextPaths;
pub use audit::{emit_expired_reservation_events, emit_task_lock_release_event};
pub use command::{
    EmptyTaskSurfacePolicy, ReservationOutcome, list, parse_task_ids, prepare_reserve,
    prepare_reserve_with_index, release, reserve_deferred_footprint,
    runs_holding_file_reservations, workspace_orbit_dir, workspace_task_reservation_id,
};
pub use index::{
    TaskLockIndex, TaskLockOverlap, active_task_lock_holders, lock_context_files_for_task,
    lock_holder_index, merge_task_lock_conflicts, requested_task_files_indexed,
    task_lock_conflicts_indexed, task_lock_overlaps,
};
pub use prepared::{PreparedGrant, STALE_OWNER_RECONCILE_LIMIT};
