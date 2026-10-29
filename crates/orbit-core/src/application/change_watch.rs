//! Observational reads for a workspace change watcher, such as the
//! dashboard's change stream: cheap stamps a caller compares between two reads
//! to tell what changed, whichever process wrote it. None of them reconciles,
//! repairs or audits, so a watcher's poll never queues behind a drain's
//! commits; each answer can be stale as soon as it returns.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::workflow::{JobRun, JobRunState};

pub use orbit_store::contracts::TaskChangeStamp;

use crate::OrbitRuntime;

/// The run input keys that name its task.
const RUN_TASK_KEYS: &[&str] = &["task_id", "task_ids"];

/// One run's state and the task it works on, when its input names one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStateStamp {
    pub run_id: String,
    pub task_id: Option<String>,
    pub state: JobRunState,
}

impl RunStateStamp {
    fn from_run(run: JobRun) -> Self {
        let task_id = run.input.as_ref().and_then(|input| {
            input
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    input
                        .get("task_ids")
                        .and_then(serde_json::Value::as_array)
                        .and_then(|ids| ids.first())
                        .and_then(serde_json::Value::as_str)
                })
                .map(ToOwned::to_owned)
        });
        Self {
            run_id: run.run_id,
            task_id,
            state: run.state,
        }
    }
}

impl OrbitRuntime {
    /// Every task of this workspace with its status and last update, from the
    /// generated index alone.
    pub fn task_change_stamps(&self) -> Result<Vec<TaskChangeStamp>, OrbitError> {
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().task_change_stamps()
    }

    /// The runs a watcher must compare: every pending or running run, plus
    /// each run whose latest activity (finish, else start, else creation) is
    /// at or after `since`. A run's state change stamps that activity, so a
    /// watcher that reads from its previous read's time sees each change.
    pub fn run_state_stamps(&self, since: DateTime<Utc>) -> Result<Vec<RunStateStamp>, OrbitError> {
        let jobs = self.stores().jobs();
        let mut runs = jobs.list_job_runs_filtered(&JobRunQuery {
            active_only: true,
            include_steps: false,
            input_keys: Some(RUN_TASK_KEYS),
            ..JobRunQuery::default()
        })?;
        runs.extend(jobs.list_job_runs_filtered(&JobRunQuery {
            recency_since: Some(since),
            include_steps: false,
            input_keys: Some(RUN_TASK_KEYS),
            ..JobRunQuery::default()
        })?);
        let mut stamps = runs
            .into_iter()
            .map(RunStateStamp::from_run)
            .collect::<Vec<_>>();
        stamps.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        stamps.dedup_by(|left, right| left.run_id == right.run_id);
        Ok(stamps)
    }

    /// One run's current stamp, `None` once it is gone.
    pub fn run_state_stamp(&self, run_id: &str) -> Result<Option<RunStateStamp>, OrbitError> {
        Ok(self
            .stores()
            .jobs()
            .get_job_run(run_id)?
            .map(RunStateStamp::from_run))
    }
}
