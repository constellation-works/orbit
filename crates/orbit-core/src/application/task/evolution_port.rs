//! Task-owned checkpoint projection, implemented by evolution policy.
//!
//! Evolution reads task comments for recovery and tasks for proposal state, so
//! task writes reach it only through this port. Composing
//! [`OrbitRuntime`](crate::OrbitRuntime) binds the implementation.

use orbit_common::OrbitError;
use orbit_types::task::TaskComment;

/// The checkpoint projection task writes request from evolution.
pub(in crate::application) trait TaskEvolutionPort {
    /// Project checkpoint costs after the comments commit, under the task's
    /// write lock. An error propagates without rolling back the durable write;
    /// the comments remain the canonical source for idempotent reconciliation.
    fn project_task_checkpoint_costs(&self, comments: &[TaskComment]) -> Result<(), OrbitError>;
}
