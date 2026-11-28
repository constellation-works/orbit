//! Auto-task's side of the job-owned auto-task port (ORB-20158).

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskHistoryEntry};

use crate::OrbitRuntime;
use crate::application::job::JobAutoTaskPort;

pub(crate) struct JobPort<'a>(pub(crate) &'a OrbitRuntime);

impl JobAutoTaskPort for JobPort<'_> {
    fn freeze_job_auto_task_run(
        &self,
        run_id: &str,
        yaml: &str,
        now: DateTime<Utc>,
    ) -> Result<(), OrbitError> {
        super::job_step::freeze_auto_task_run(self.0, run_id, yaml, now)
    }

    fn step_bound_start_hold(
        &self,
        task: &Task,
        history: &dyn Fn() -> Result<Vec<TaskHistoryEntry>, OrbitError>,
    ) -> Option<String> {
        super::job_step::step_bound_admission_hold(self.0, task, history)
    }
}
