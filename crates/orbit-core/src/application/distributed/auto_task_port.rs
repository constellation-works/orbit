//! What pull admission needs from auto-task job steps (ORB-20158).
//!
//! A task an auto-task job step minted is claimed only while its step still
//! admits it. `app::auto_task` owns that rule and reads tasks and runs back,
//! so a direct call from here would name a higher module. This port is the
//! way in: `app::auto_task` implements it and `app::port` binds the adapter.

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskHistoryEntry};

/// The step-bound admission rule implemented by `auto_task`.
pub(crate) trait DistributedAutoTaskPort {
    /// Why a task an auto-task step minted may not be claimed now; `None`
    /// for every other task. History is read only for a task carrying an
    /// auto-task tag. Implemented by auto_task.
    fn step_bound_claim_hold(
        &self,
        task: &Task,
        history: &dyn Fn() -> Result<Vec<TaskHistoryEntry>, OrbitError>,
    ) -> Option<String>;
}
