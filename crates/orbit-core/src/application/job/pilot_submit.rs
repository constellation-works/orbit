//! Operator submission of one explicit task-pilot run.
//!
//! The dashboard's "Run pilot" action and `orbit run job task_pilot_pipeline
//! --input 'task_ids=["<id>"]'` are the same submission: this module only adds
//! the refusals an operator surface wants *before* a run exists, each typed so
//! the surface can name the reason. The pipeline's prepare step repeats the
//! status and holder checks against current state, so a race between the
//! preflight and the run still settles safely there. Piloting never promotes
//! or dispatches the task.

use std::fmt;

use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{JobRunState, JobRunTrigger};
use serde_json::json;

use super::automation_port::JobAutomationPort;
use super::{JobRunListParams, PipelineInvokeResult};
use crate::OrbitRuntime;
use crate::application::task::pilot_assessable_status;

const PILOT_JOB: &str = "task_pilot_pipeline";
/// Bound on the live pilot runs scanned for one task; non-terminal runs are
/// always among the newest rows.
const PILOT_SCAN_LIMIT: usize = 50;

/// Why an explicit pilot submission was refused before any run was created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskPilotRefusal {
    /// The task is not `proposed` or `backlog`: it is admitted, in flight,
    /// finished or retired.
    UnsupportedStatus { task_id: String, status: TaskStatus },
    /// A live pilot run already holds the task.
    PilotRunning { task_id: String, run_id: String },
    /// A live delivery run carries the task, so it is admitted or claimed.
    DeliveryRunning { task_id: String, run_id: String },
    /// This checkout is a replica (follower); the owner machine pilots.
    ReplicaCheckout { owner: String },
}

impl TaskPilotRefusal {
    /// Stable machine-readable token for the refusal.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedStatus { .. } => "task_pilot_unsupported_status",
            Self::PilotRunning { .. } => "task_pilot_already_running",
            Self::DeliveryRunning { .. } => "task_pilot_delivery_running",
            Self::ReplicaCheckout { .. } => "task_pilot_replica_checkout",
        }
    }
}

impl fmt::Display for TaskPilotRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedStatus { task_id, status } => write!(
                f,
                "explicit task-pilot selection refuses {task_id} (status {status}); only proposed and backlog tasks may be assessed"
            ),
            Self::PilotRunning { task_id, run_id } => write!(
                f,
                "task {task_id} is already held by pilot run {run_id}; wait for it to finish or cancel it"
            ),
            Self::DeliveryRunning { task_id, run_id } => write!(
                f,
                "task {task_id} is admitted to delivery run {run_id}; wait for it to finish or cancel it"
            ),
            Self::ReplicaCheckout { owner } => write!(
                f,
                "this checkout is a replica of owner machine {owner}; run the pilot on the owner"
            ),
        }
    }
}

/// A failed explicit pilot submission.
#[derive(Debug)]
pub enum TaskPilotSubmitError {
    /// The preflight refused the request; nothing was submitted.
    Refused(TaskPilotRefusal),
    /// Lookup or submission failed (an unknown task is `NotFound`).
    Runtime(OrbitError),
}

impl fmt::Display for TaskPilotSubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(f),
            Self::Runtime(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for TaskPilotSubmitError {}

impl From<OrbitError> for TaskPilotSubmitError {
    fn from(error: OrbitError) -> Self {
        Self::Runtime(error)
    }
}

impl OrbitRuntime {
    /// Submit `task_pilot_pipeline` in explicit mode for exactly `task_id`,
    /// through the shared catalog submission path.
    pub fn submit_explicit_task_pilot(
        &self,
        task_id: &str,
        actor: Option<&str>,
        trigger: JobRunTrigger,
    ) -> Result<PipelineInvokeResult, TaskPilotSubmitError> {
        if let Some(owner) = self.replica_owner_machine() {
            return Err(TaskPilotSubmitError::Refused(
                TaskPilotRefusal::ReplicaCheckout {
                    owner: owner.to_string(),
                },
            ));
        }
        let task = self.get_task(task_id)?;
        if !pilot_assessable_status(task.status) {
            return Err(TaskPilotSubmitError::Refused(
                TaskPilotRefusal::UnsupportedStatus {
                    task_id: task.id,
                    status: task.status,
                },
            ));
        }
        if let Some(run_id) = self.live_pilot_run_for_task(&task.id)? {
            return Err(TaskPilotSubmitError::Refused(
                TaskPilotRefusal::PilotRunning {
                    task_id: task.id,
                    run_id,
                },
            ));
        }
        if let Some(OrbitError::ShipRunInFlight { task_id, run_id }) =
            self.in_flight_ship_run_for_tasks(std::slice::from_ref(&task.id))?
        {
            return Err(TaskPilotSubmitError::Refused(
                TaskPilotRefusal::DeliveryRunning { task_id, run_id },
            ));
        }
        Ok(self.submit_catalog_job_run(
            PILOT_JOB,
            json!({ "task_ids": [task.id] }),
            actor,
            trigger,
        )?)
    }

    /// The live pilot run holding `task_id`: one that names it in its input,
    /// or whose reservation or prepare checkpoint holds it.
    fn live_pilot_run_for_task(&self, task_id: &str) -> Result<Option<String>, OrbitError> {
        let named = self.list_job_runs(JobRunListParams {
            job_id: Some(PILOT_JOB.to_string()),
            task_id: Some(task_id.to_string()),
            states: vec![JobRunState::Pending, JobRunState::Running],
            limit: Some(PILOT_SCAN_LIMIT),
            ..Default::default()
        })?;
        if let Some(run) = named.into_iter().next() {
            return Ok(Some(run.run_id));
        }
        self.pilot_holder_run(task_id)
    }
}
