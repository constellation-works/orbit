//! [`JobRuns`]: one workspace's job runs, local pull admissions and drain
//! intent, and the run lifecycle rules that decide each write.
//!
//! The records live in orbit-store's `job_run`, `local_pull` and
//! `drain_intent` repositories. Every read here opens one [`Store::read`]
//! snapshot; every write runs its rule inside one [`Store::write`]
//! transaction, so a decision and the write it makes commit together and the
//! process-wide SQLite writer lock orders competing callers in every process.
//!
//! `admit` inserts runs (top-level, resume, keyed, automation and child),
//! `lifecycle` moves a run through start, steps and its terminal write,
//! `state` reads and writes pipeline state, and `pull` holds the local pull
//! admission rules and the shared drain capacity reading.

mod admit;
mod lifecycle;
mod pull;
mod state;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path};
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_store::Store;
use orbit_store::contract::{DrainIntentRecord, JobRunQuery, JobRunStepTargetQuery};
use orbit_types::task::ExecutionLocation;
use orbit_types::workflow::{JobRun, JobRunState, PipelineState};

/// One workspace partition of the host store's job runs.
///
/// Newly minted run ids are unique across the host's shared store, but
/// historical run ids can collide across workspaces, so every read and write
/// stays in [`Self::workspace_id`]'s partition.
#[derive(Clone)]
pub struct JobRuns {
    store: Store,
    workspace_id: String,
    /// Trusted runtime identity stamped on runs this handle inserts. Neither
    /// caller input nor updates to existing runs can rewrite a run's origin.
    executed_on: Option<ExecutionLocation>,
}

impl JobRuns {
    /// `workspace_id`'s runs in `store`, inserting runs with no execution
    /// location.
    pub fn new(store: Store, workspace_id: impl Into<String>) -> Self {
        Self {
            store,
            workspace_id: workspace_id.into(),
            executed_on: None,
        }
    }

    /// [`Self::new`], shared.
    pub fn shared(store: Store, workspace_id: impl Into<String>) -> Arc<Self> {
        Arc::new(Self::new(store, workspace_id))
    }

    /// This handle, stamping `location` on future insertions only.
    pub fn with_execution_location(&self, location: Option<ExecutionLocation>) -> Arc<Self> {
        Arc::new(Self {
            executed_on: location,
            ..self.clone()
        })
    }

    /// The workspace partition this handle reads and writes runs under.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// The host store the runs live in.
    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        self.store.read()?.job_run().get(&self.workspace_id, run_id)
    }

    /// Steps-free run rows for `run_ids`. Missing ids are omitted and each
    /// `input` keeps only the task-label keys `task_id` and `task_ids`.
    pub fn get_job_runs_by_id(&self, run_ids: &[String]) -> Result<Vec<JobRun>, OrbitError> {
        self.store
            .read()?
            .job_run()
            .get_many(&self.workspace_id, run_ids)
    }

    /// Only the run's lifecycle state: no step rows, no input and no
    /// pipeline state.
    pub fn get_job_run_state(&self, run_id: &str) -> Result<Option<JobRunState>, OrbitError> {
        self.store
            .read()?
            .job_run()
            .get_state(&self.workspace_id, run_id)
    }

    pub fn list_job_runs(&self, job_id: &str) -> Result<Vec<JobRun>, OrbitError> {
        validate_job_id(job_id)?;
        self.list_job_runs_filtered(&JobRunQuery {
            job_id: Some(job_id.to_string()),
            ..Default::default()
        })
    }

    pub fn list_job_runs_filtered(&self, query: &JobRunQuery) -> Result<Vec<JobRun>, OrbitError> {
        self.store.read()?.job_run().list(&self.workspace_id, query)
    }

    /// Bounded runs matching `query`, hydrating only matching steps for
    /// `query.step_target_id` without decoding `agent_response_json`.
    pub fn list_job_runs_with_step_target(
        &self,
        query: &JobRunStepTargetQuery,
    ) -> Result<Vec<JobRun>, OrbitError> {
        self.store
            .read()?
            .job_run()
            .list_with_step_target(&self.workspace_id, query)
    }

    /// Up to `per_task_limit` recent runs of `job_id` for each requested
    /// task, ordered by recency descending, then run id ascending, including
    /// steps. Ownership uses the exact top-level text bindings of
    /// `JobRunQuery.task_id`; missing tasks are omitted, and limits apply per
    /// task before hydration.
    pub fn recent_job_runs_for_tasks(
        &self,
        job_id: &str,
        task_ids: &[String],
        per_task_limit: usize,
    ) -> Result<BTreeMap<String, Vec<JobRun>>, OrbitError> {
        self.store.read()?.job_run().list_recent_for_tasks(
            &self.workspace_id,
            job_id,
            task_ids,
            per_task_limit,
        )
    }

    /// Distinct trimmed, nonblank task ids carried by the selected runs'
    /// top-level `task_ids` string array or text `task_id`. Run filters and
    /// limits apply before projection.
    pub fn job_run_task_ids(&self, query: &JobRunQuery) -> Result<BTreeSet<String>, OrbitError> {
        self.store
            .read()?
            .job_run()
            .list_task_ids(&self.workspace_id, query)
    }

    /// Latest created run, including steps, for each distinct requested job,
    /// ordered by job id. Missing jobs are omitted.
    pub fn latest_job_runs(&self, job_ids: &[String]) -> Result<Vec<JobRun>, OrbitError> {
        self.store
            .read()?
            .job_run()
            .list_latest(&self.workspace_id, job_ids)
    }

    /// Number of runs matching `query`, ignoring its `limit`.
    pub fn count_job_runs_filtered(&self, query: &JobRunQuery) -> Result<u64, OrbitError> {
        self.store
            .read()?
            .job_run()
            .count(&self.workspace_id, query)
    }

    /// Every recorded `duration_ms` among runs matching `query`, ignoring its
    /// `limit`.
    pub fn list_job_run_durations_filtered(
        &self,
        query: &JobRunQuery,
    ) -> Result<Vec<u64>, OrbitError> {
        self.store
            .read()?
            .job_run()
            .list_durations(&self.workspace_id, query)
    }

    pub fn list_pending_or_running_job_runs(
        &self,
        job_id: &str,
    ) -> Result<Vec<JobRun>, OrbitError> {
        validate_job_id(job_id)?;
        self.list_job_runs_filtered(&JobRunQuery {
            job_id: Some(job_id.to_string()),
            active_only: true,
            ..Default::default()
        })
    }

    /// Every live run of the workspace, without steps.
    pub fn list_all_pending_or_running_runs(&self) -> Result<Vec<JobRun>, OrbitError> {
        // No consumer of this listing reads step rows: stale-run
        // reconciliation classifies from run-row fields and re-reads the
        // full run before finalizing, and the orphan/parked probes read
        // only state and owner fields.
        self.list_job_runs_filtered(&JobRunQuery {
            active_only: true,
            include_steps: false,
            ..Default::default()
        })
    }

    /// Exact retry children of `run_id`, oldest first, at most 1000;
    /// missing evidence cannot be replaced by a time-window scan.
    pub fn job_run_retries(&self, run_id: &str, limit: usize) -> Result<Vec<JobRun>, OrbitError> {
        self.store
            .read()?
            .job_run()
            .list_retries(&self.workspace_id, run_id, limit)
    }

    /// The run bound to automation action `key`.
    pub fn automation_job_for_key(&self, key: &str) -> Result<Option<String>, OrbitError> {
        self.store
            .read()?
            .job_run()
            .get_automation_key(&self.workspace_id, key)
    }

    pub fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError> {
        self.store
            .read()?
            .job_run()
            .get_pipeline_state(&self.workspace_id, run_id)
    }

    /// The run's serialized pipeline state, unparsed and as stored, for a
    /// reader that needs only a projection of it. Unreadable JSON is the
    /// caller's to classify.
    pub fn read_run_state_json(&self, run_id: &str) -> Result<Option<String>, OrbitError> {
        self.store
            .read()?
            .job_run()
            .get_pipeline_state_json(&self.workspace_id, run_id)
    }

    /// [`Self::read_run_state_json`] for many runs in one query per chunk. A
    /// run with no state row, or no run, maps to `None`.
    pub fn read_run_state_jsons(
        &self,
        run_ids: &[String],
    ) -> Result<HashMap<String, Option<String>>, OrbitError> {
        self.store
            .read()?
            .job_run()
            .get_pipeline_state_jsons(&self.workspace_id, run_ids)
    }

    /// Pipeline state for a list page in one query per chunk. Missing runs
    /// are absent and unreadable JSON is `None` rather than failing the page.
    pub fn read_run_states(
        &self,
        run_ids: &[String],
    ) -> Result<HashMap<String, Option<PipelineState>>, OrbitError> {
        self.store
            .read()?
            .job_run()
            .get_pipeline_states(&self.workspace_id, run_ids)
    }

    /// Pipeline state for many runs in one store call, failing on unreadable
    /// JSON with the error [`Self::read_run_state`] returns. A run with no
    /// state row is `None`; a run that does not exist is absent.
    pub fn read_run_states_exact(
        &self,
        run_ids: &[String],
    ) -> Result<HashMap<String, Option<PipelineState>>, OrbitError> {
        self.store
            .read()?
            .job_run()
            .get_pipeline_states_exact(&self.workspace_id, run_ids)
    }

    /// This workspace's recorded drain intent; `None` is the default `off`.
    pub fn drain_intent(&self) -> Result<Option<DrainIntentRecord>, OrbitError> {
        self.store.read()?.drain_intent().get(&self.workspace_id)
    }

    /// Replace this workspace's drain intent with `record`.
    pub fn record_drain_intent(&self, record: &DrainIntentRecord) -> Result<(), OrbitError> {
        self.store
            .write(|tx| tx.drain_intent().put(&self.workspace_id, record))
    }
}

/// A job id names a definition file, so it must be one path component.
fn validate_job_id(job_id: &str) -> Result<(), OrbitError> {
    let mut components = Path::new(job_id).components();
    if matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(part)), None) if part.to_str() == Some(job_id)
    ) {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "job id must be a single path component without separators or traversal: {job_id}"
    )))
}
