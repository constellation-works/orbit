//! The job-run store contract: run lifecycle, pull admission, child-run
//! admission and pipeline steps.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::identity::Crew;
use orbit_types::workflow::{
    JobRun, JobRunStartOutcome, JobRunState, KnowledgeRunMetrics, PipelineState, RunStateUpdate,
};
use serde_json::Value;
use std::collections::HashMap;

use super::params::*;

pub trait JobRunStoreBackend: Send + Sync {
    /// Local permanent binding, independent of owner connectivity.
    fn local_pull_for_run(&self, _run_id: &str) -> Result<Option<LocalPullAdmission>, OrbitError> {
        Ok(None)
    }
    /// Reserve local capacity and persist the request before transport.
    fn allocate_pull_request(
        &self,
        _destination: &PullDestination,
        _request: &super::AdmissionRequest,
        _ceiling: usize,
    ) -> Result<Option<LocalPullAdmission>, OrbitError> {
        Err(OrbitError::Store(
            "local pull persistence unavailable".into(),
        ))
    }

    fn local_pull_admissions(&self) -> Result<Vec<LocalPullAdmission>, OrbitError> {
        Err(OrbitError::Store(
            "local pull persistence unavailable".into(),
        ))
    }

    /// How much leaf capacity both admission paths are already using
    /// [ORB-12617].
    ///
    /// The legacy drain and the pull drain allocate against one ceiling, so
    /// they must read one number: a wrapper is replaced by the leaf runs it
    /// dispatched rather than counted beside them, every leaf definition
    /// counts, and a pending admission with no live run of its own counts too.
    /// Read-only, and it never creates pull schema in a workspace that has
    /// none.
    fn drain_leaf_occupancy(&self) -> Result<super::DrainLeafOccupancy, OrbitError> {
        Err(OrbitError::Store("drain leaf occupancy unavailable".into()))
    }

    fn mutate_local_pull(
        &self,
        _destination: &PullDestination,
        _request_id: &str,
        _mutation: &LocalPullMutation,
    ) -> Result<LocalPullAdmission, OrbitError> {
        Err(OrbitError::Store(
            "local pull persistence unavailable".into(),
        ))
    }

    /// Configure trusted runtime identity for future insertions only. Neither
    /// caller input nor updates to existing runs can rewrite their origin.
    fn with_execution_location(
        &self,
        location: Option<orbit_types::task::ExecutionLocation>,
    ) -> std::sync::Arc<dyn JobRunStoreBackend>;

    /// Exact retry children; missing evidence cannot be replaced by a time-window scan.
    fn job_run_retries(&self, _run_id: &str, _limit: usize) -> Result<Vec<JobRun>, OrbitError> {
        Err(OrbitError::Store("retry lineage lookup unavailable".into()))
    }

    fn automation_job_for_key(&self, _key: &str) -> Result<Option<String>, OrbitError> {
        Err(OrbitError::Store(
            "automation action lookup unavailable".into(),
        ))
    }
    fn insert_automation_job_run(
        &self,
        _job_id: &str,
        _input: serde_json::Value,
        _key: &str,
    ) -> Result<JobRun, OrbitError> {
        Err(OrbitError::Store(
            "automation job admission unavailable".into(),
        ))
    }

    fn list_job_runs(&self, job_id: &str) -> Result<Vec<JobRun>, OrbitError>;
    fn list_job_runs_filtered(&self, query: &JobRunQuery) -> Result<Vec<JobRun>, OrbitError>;
    /// Number of runs matching `query`, ignoring its `limit`.
    fn count_job_runs_filtered(&self, query: &JobRunQuery) -> Result<u64, OrbitError>;
    /// Every recorded `duration_ms` among runs matching `query`, ignoring
    /// its `limit`.
    fn list_job_run_durations_filtered(&self, query: &JobRunQuery) -> Result<Vec<u64>, OrbitError>;
    fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError>;
    fn list_pending_or_running_job_runs(&self, job_id: &str) -> Result<Vec<JobRun>, OrbitError>;
    fn insert_job_run(
        &self,
        job_id: &str,
        attempt: u32,
        scheduled_at: DateTime<Utc>,
        input: Option<serde_json::Value>,
        retry_source_run_id: Option<String>,
    ) -> Result<JobRun, OrbitError>;
    /// Atomically admit a resume of `retry_source_run_id` unless its retry
    /// lineage already has a live run.
    ///
    /// The lineage is the source's `retry_source_run_id` ancestors and every
    /// run descended from any of them: all of them reuse the same checkpointed
    /// worktree and task claims. When one is `pending`, `running`, or
    /// `retrying`, this inserts nothing and fails with
    /// [`OrbitError::ResumeRunInFlight`] naming the oldest such run. The
    /// lineage read and the insert share one immediate transaction, so
    /// concurrent resumes of one lineage from any process admit exactly one.
    fn insert_resume_job_run(
        &self,
        _job_id: &str,
        _attempt: u32,
        _scheduled_at: DateTime<Utc>,
        _input: Option<serde_json::Value>,
        _retry_source_run_id: &str,
    ) -> Result<JobRun, OrbitError> {
        Err(OrbitError::Store("resume admission unavailable".into()))
    }
    /// Atomically admit and link a child run unless its parent has stopped
    /// admissions.
    ///
    /// The parent-state read, child insert, and parent dispatch checkpoint are
    /// committed in one backend transaction. That commit is the
    /// cross-process linearization point shared with an admissions-stop
    /// update: a stop that commits first makes this return
    /// [`ChildJobRunAdmissionOutcome::AdmissionsStopped`], while a child that
    /// commits first is already linked when stop acknowledges.
    fn admit_child_job_run(
        &self,
        params: &ChildJobRunAdmissionParams,
    ) -> Result<ChildJobRunAdmissionOutcome, OrbitError>;
    /// [ORB-10965] Apply a `Start` event to a run, atomically and idempotently.
    ///
    /// Scheduling is at-least-once, so this is the single point that decides
    /// which of several competing or repeated deliveries owns execution. The
    /// read of the current state and the write of the new one happen in one
    /// immediate transaction, so exactly one caller can observe
    /// [`JobRunStartOutcome::Started`].
    ///
    /// A redelivery from the owner already recorded on the run is a no-op:
    /// [`JobRunStartOutcome::AlreadyStarted`], with `started_at`, the owner
    /// identity, and every checkpoint left untouched. A delivery from a
    /// *different* owner loses to the incumbent and fails with
    /// [`OrbitError::JobRunStartConflict`]. Genuinely illegal transitions (a
    /// `Start` from any state other than `pending`, `running`, or terminal)
    /// still fail with [`OrbitError::JobRunStateTransition`].
    fn mark_job_run_running(
        &self,
        run_id: &str,
        started_at: DateTime<Utc>,
        pid: u32,
    ) -> Result<JobRunStartOutcome, OrbitError>;
    /// [ORB-10070] Record `pid` (+ its start-time identity token) as the owner
    /// of a still-`pending` run so orphan reconciliation can distinguish a
    /// queued run with a live worker from one whose worker died. Returns
    /// `false` without writing when the run is missing or no longer pending.
    fn claim_pending_job_run_owner(&self, run_id: &str, pid: u32) -> Result<bool, OrbitError>;
    fn complete_job_run_step(
        &self,
        run_id: &str,
        params: &JobRunStepParams,
    ) -> Result<bool, OrbitError>;
    fn record_job_run_knowledge_metrics(
        &self,
        run_id: &str,
        metrics: KnowledgeRunMetrics,
    ) -> Result<bool, OrbitError>;
    fn record_job_run_crew(&self, run_id: &str, crew: &Crew) -> Result<bool, OrbitError>;
    fn finalize_job_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError>;
    fn repair_terminal_job_run_timing(
        &self,
        run_id: &str,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError>;
    fn list_all_pending_or_running_runs(&self) -> Result<Vec<JobRun>, OrbitError>;
    fn archive_job_run(&self, run_id: &str) -> Result<String, OrbitError>;
    fn delete_job_run(&self, run_id: &str) -> Result<String, OrbitError>;
    fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError>;
    /// Pipeline state for a list page in one query per chunk, not one per run.
    ///
    /// Missing runs and unreadable JSON are omitted / `None` rather than failing
    /// the page: the MCP list surface degrades those rows the same way a
    /// per-run `read_run_state` error already does.
    fn read_run_states(
        &self,
        run_ids: &[String],
    ) -> Result<HashMap<String, Option<PipelineState>>, OrbitError>;
    fn write_run_state(&self, run_id: &str, state: &PipelineState) -> Result<(), OrbitError>;
    /// [ORB-11253] Read-modify-write a run's pipeline state in one immediate
    /// transaction.
    ///
    /// The plain read/write pair cannot express a change that must survive
    /// another writer: the run's state is one document that the engine
    /// (checkpoints, child dispatches) and operator run controls both mutate,
    /// so two interleaved read-modify-write cycles silently drop whichever
    /// change landed in between. `update` also receives the run's current
    /// [`JobRunState`], so a caller that must not mutate a finished run can
    /// refuse inside the same transaction that would otherwise have written.
    /// An `Err` from `update` rolls the transaction back, leaving the stored
    /// state exactly as it was.
    fn update_run_state(
        &self,
        run_id: &str,
        update: &mut dyn FnMut(JobRunState, &mut PipelineState) -> Result<(), OrbitError>,
    ) -> Result<RunStateUpdate, OrbitError>;
}

/// Durable inputs for one parent-authorized child admission.
#[derive(Debug, Clone)]
pub struct ChildJobRunAdmissionParams {
    pub parent_run_id: String,
    pub parent_step_id: Option<String>,
    pub job_id: String,
    pub action: String,
    pub blocking: bool,
    pub attempt: u32,
    pub scheduled_at: DateTime<Utc>,
    pub input: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChildJobRunAdmissionOutcome {
    Admitted(Box<JobRun>),
    AdmissionsStopped,
}

#[derive(Debug, Clone)]
pub struct JobRunStepParams {
    pub step_index: usize,
    pub target_type: orbit_types::workflow::JobTargetType,
    pub target_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub agent_response_json: Option<Value>,
    pub state: JobRunState,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}
