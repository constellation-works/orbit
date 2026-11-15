//! Observation-only reads of one run by id across every workspace id this
//! host's store records runs under, including workspaces no longer
//! registered on the host.
//!
//! The host store (`<global_root>/orbit.db`) keys runs by
//! `(workspace_id, run_id)`. A runtime reads only its own workspace's rows,
//! so a run a host executed for a workspace it later removed (a pull
//! follower's claimed leaf after its replica checkout was unregistered) is
//! still on disk but no registered runtime resolves it. These reads let a
//! run-detail surface find such a run and say where an absent one is held.

use orbit_common::OrbitError;
use orbit_store::compose::workspace_job_run_store;
use orbit_types::task::ExecutionLocation;
use orbit_types::workflow::{JobRun, PipelineState};

use crate::OrbitRuntime;

/// A run read from this host's store under `workspace_id`, with its
/// pipeline state, exactly as stored (no reconciliation).
#[derive(Debug, Clone)]
pub struct RecordedJobRun {
    pub workspace_id: String,
    pub run: JobRun,
    pub state: Option<PipelineState>,
}

/// What this runtime's tasks record about where a run executed: the task
/// naming the run and the execution machine bound with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRunHolder {
    pub task_id: String,
    /// `None` when the binding recorded no machine.
    pub location: Option<ExecutionLocation>,
    /// Whether `location` is this host's own machine identity.
    pub local: bool,
}

impl OrbitRuntime {
    /// Every workspace id this host's store records `run_id` under,
    /// ascending, registered on this host or not.
    pub fn job_run_recording_workspaces(&self, run_id: &str) -> Result<Vec<String>, OrbitError> {
        self.sqlite_store()?.job_run_workspace_ids(run_id)
    }

    /// `run_id` as this host's store records it under `workspace_id`, which
    /// need not be this runtime's workspace nor a registered one. Never
    /// reconciles or writes: the workspace may have no live runtime that
    /// could own a correction.
    pub fn recorded_job_run(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Option<RecordedJobRun>, OrbitError> {
        let jobs = workspace_job_run_store(self.sqlite_store()?, workspace_id);
        let Some(run) = jobs.get_job_run(run_id)? else {
            return Ok(None);
        };
        let state = jobs.read_run_state(run_id)?;
        Ok(Some(RecordedJobRun {
            workspace_id: workspace_id.to_string(),
            run,
            state,
        }))
    }

    /// Where this workspace's tasks say `run_id` executed, for a run this
    /// host's store does not hold. A task bound with a recorded machine wins
    /// over one without; `None` when no task names the run.
    pub fn job_run_holder(&self, run_id: &str) -> Result<Option<JobRunHolder>, OrbitError> {
        let mut tasks = self.list_tasks_filtered(None, None, None, Some(run_id), None, None)?;
        tasks.sort_by_key(|task| task.job_run_machine.is_none());
        let local = self.automation_machine_identity();
        Ok(tasks.into_iter().next().map(|task| JobRunHolder {
            local: task
                .job_run_machine
                .as_ref()
                .is_some_and(|location| local == Some(location.machine_id.as_str())),
            task_id: task.id,
            location: task.job_run_machine,
        }))
    }
}
