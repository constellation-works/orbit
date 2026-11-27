//! What job admission needs from auto-task job steps (ORB-20158).
//!
//! An admitted run of a job with auto-task steps is frozen before it starts,
//! and a task an auto-task step minted starts only while its step still
//! admits it. `app::auto_task` owns both and reads tasks and runs back, so a
//! direct call from here would name a higher module. This port is the way
//! in: `app::auto_task` implements it and `app::port` binds the adapter.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskHistoryEntry};

/// The auto-task job-step operations job admission calls, implemented by
/// `auto_task`.
pub(crate) trait JobAutoTaskPort {
    /// Freeze an admitted execution of a job with auto-task steps: the
    /// definition it runs, its admission time, its outer deadline and any
    /// pinned integration revision. Implemented by auto_task.
    fn freeze_job_auto_task_run(
        &self,
        run_id: &str,
        yaml: &str,
        now: DateTime<Utc>,
    ) -> Result<(), OrbitError>;

    /// Why a task an auto-task step minted may not start now; `None` for
    /// every other task. Implemented by auto_task.
    fn step_bound_start_hold(
        &self,
        task: &Task,
        history: &dyn Fn() -> Result<Vec<TaskHistoryEntry>, OrbitError>,
    ) -> Option<String>;
}
