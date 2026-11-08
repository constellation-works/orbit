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

use orbit_common::{OrbitError, TaskPilotRefusalCode};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{JobRunState, JobRunTrigger};
use serde::Serialize;
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
    /// The task is assigned to a human crew: operator work no pilot assesses.
    HumanCrew { task_id: String, crew: String },
    /// A live pilot run already holds the task.
    PilotRunning { task_id: String, run_id: String },
    /// A live delivery run carries the task, so it is admitted or claimed.
    DeliveryRunning { task_id: String, run_id: String },
    /// This checkout is a replica (follower); the owner machine pilots.
    ReplicaCheckout { owner: String },
}

impl TaskPilotRefusal {
    /// The typed refusal code every surface reports.
    pub fn refusal_code(&self) -> TaskPilotRefusalCode {
        match self {
            Self::UnsupportedStatus { .. } => TaskPilotRefusalCode::UnsupportedStatus,
            Self::HumanCrew { .. } => TaskPilotRefusalCode::HumanCrew,
            Self::PilotRunning { .. } => TaskPilotRefusalCode::AlreadyRunning,
            Self::DeliveryRunning { .. } => TaskPilotRefusalCode::DeliveryRunning,
            Self::ReplicaCheckout { .. } => TaskPilotRefusalCode::ReplicaCheckout,
        }
    }

    /// Stable machine-readable token for the refusal.
    pub fn code(&self) -> &'static str {
        self.refusal_code().as_str()
    }
}

impl fmt::Display for TaskPilotRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedStatus { task_id, status } => write!(
                f,
                "explicit task-pilot selection refuses {task_id} (status {status}); only proposed and backlog tasks may be assessed"
            ),
            Self::HumanCrew { task_id, crew } => write!(
                f,
                "task {task_id} is assigned to human crew {crew}: operator work the task pilot never assesses; reassign it to an agent crew first"
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

/// The one error mapping every surface uses: a refusal becomes
/// [`OrbitError::TaskPilotRefused`] carrying its stable code, and a runtime
/// error keeps its own kind (an unknown task stays `NotFound`).
impl From<TaskPilotSubmitError> for OrbitError {
    fn from(error: TaskPilotSubmitError) -> Self {
        match error {
            TaskPilotSubmitError::Refused(refusal) => {
                Self::task_pilot_refused(refusal.refusal_code(), refusal.to_string())
            }
            TaskPilotSubmitError::Runtime(error) => error,
        }
    }
}

/// The result of one explicit pilot submission, rendered by every surface.
///
/// `orbit.task.repilot` (CLI, MCP, remote) serializes it as is. The
/// dashboard's `POST /api/tasks/<id>/pilot` renders [`Self::dashboard_view`],
/// which publishes the same values under its own established names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskPilotSubmission {
    pub task_id: String,
    pub run_id: String,
    /// The submitted job. Published as `job_name` by the tool and as its
    /// alias `job_id` by the dashboard; both names are public contract.
    pub job_name: String,
    pub submitted_at: String,
    /// Whether the run waits for a concurrency slot. The dashboard publishes
    /// it as `state` (`queued` or `submitted`).
    pub queued: bool,
    /// One-based position among runs waiting for a concurrency slot.
    pub queue_position: Option<usize>,
}

/// The dashboard's rendering of a [`TaskPilotSubmission`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskPilotSubmissionDashboardView<'a> {
    pub task_id: &'a str,
    /// Alias of [`TaskPilotSubmission::job_name`].
    pub job_id: &'a str,
    pub run_id: &'a str,
    /// `queued` or `submitted`, from [`TaskPilotSubmission::queued`].
    pub state: &'static str,
    pub submitted_at: &'a str,
    pub queue_position: Option<usize>,
}

impl TaskPilotSubmission {
    fn from_invoke(task_id: &str, invoke: PipelineInvokeResult) -> Self {
        Self {
            task_id: task_id.to_string(),
            run_id: invoke.run_id,
            job_name: invoke.job_name,
            submitted_at: invoke.submitted_at,
            queued: invoke.queued,
            queue_position: invoke.queue_position,
        }
    }

    /// The dashboard's field names for this result.
    pub fn dashboard_view(&self) -> TaskPilotSubmissionDashboardView<'_> {
        TaskPilotSubmissionDashboardView {
            task_id: &self.task_id,
            job_id: &self.job_name,
            run_id: &self.run_id,
            state: if self.queued { "queued" } else { "submitted" },
            submitted_at: &self.submitted_at,
            queue_position: self.queue_position,
        }
    }
}

impl OrbitRuntime {
    /// Submit `task_pilot_pipeline` in explicit mode for exactly `task_id`,
    /// through the shared catalog submission path.
    ///
    /// The dashboard offers no full re-pilot control, so it submits through
    /// this entry; that it never asks for `full` is intentional.
    ///
    /// Admission records `trigger` on the run. That host-owned provenance,
    /// not an agent-supplied flag, is how a later drain tells this request
    /// from one of its own children.
    pub fn submit_explicit_task_pilot(
        &self,
        task_id: &str,
        actor: Option<&str>,
        trigger: JobRunTrigger,
    ) -> Result<TaskPilotSubmission, TaskPilotSubmitError> {
        self.submit_explicit_task_pilot_in_mode(task_id, actor, trigger, false)
    }

    /// [`Self::submit_explicit_task_pilot`], where `full` forces a full
    /// assessment even when the latest one could be re-checked.
    pub fn submit_explicit_task_pilot_in_mode(
        &self,
        task_id: &str,
        actor: Option<&str>,
        trigger: JobRunTrigger,
        full: bool,
    ) -> Result<TaskPilotSubmission, TaskPilotSubmitError> {
        if let Some(owner) = self.replica_owner_machine() {
            return Err(TaskPilotSubmitError::Refused(
                TaskPilotRefusal::ReplicaCheckout {
                    owner: owner.to_string(),
                },
            ));
        }
        let mut submitted = None;
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                submitted = Some(self.check_explicit_task_pilot(task_id).and_then(|()| {
                    self.submit_catalog_job_run(
                        PILOT_JOB,
                        if full {
                            json!({"task_ids": [task_id], "full_pilot": true})
                        } else {
                            json!({"task_ids": [task_id]})
                        },
                        actor,
                        trigger.clone(),
                    )
                    .map_err(TaskPilotSubmitError::from)
                }));
                Ok(())
            })?;
        let invoke = submitted.ok_or_else(|| {
            TaskPilotSubmitError::Runtime(OrbitError::Execution(
                "pilot submission did not run".into(),
            ))
        })??;
        Ok(TaskPilotSubmission::from_invoke(task_id, invoke))
    }

    /// Check an explicit request before its accompanying task edit is written.
    pub(crate) fn check_explicit_task_pilot(
        &self,
        task_id: &str,
    ) -> Result<(), TaskPilotSubmitError> {
        self.check_task_pilot_request(task_id, false)
    }

    /// Decision recovery may return a blocked task to backlog before submission.
    pub(crate) fn check_decision_task_pilot(
        &self,
        task_id: &str,
    ) -> Result<(), TaskPilotSubmitError> {
        self.check_task_pilot_request(task_id, true)
    }

    fn check_task_pilot_request(
        &self,
        task_id: &str,
        allow_blocked: bool,
    ) -> Result<(), TaskPilotSubmitError> {
        if let Some(owner) = self.replica_owner_machine() {
            return Err(TaskPilotSubmitError::Refused(
                TaskPilotRefusal::ReplicaCheckout {
                    owner: owner.to_string(),
                },
            ));
        }
        let task = self.get_task(task_id)?;
        if !pilot_assessable_status(task.status)
            && !(allow_blocked && task.status == TaskStatus::Blocked)
        {
            return Err(TaskPilotSubmitError::Refused(
                TaskPilotRefusal::UnsupportedStatus {
                    task_id: task.id,
                    status: task.status,
                },
            ));
        }
        if let Some(human) = self.human_crew(task.crew.as_deref()) {
            return Err(TaskPilotSubmitError::Refused(TaskPilotRefusal::HumanCrew {
                crew: human.name.clone(),
                task_id: task.id,
            }));
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
        self.show_job_catalog_entry(PILOT_JOB)?;
        Ok(())
    }

    /// The live pilot run holding `task_id`: one that names it in its input,
    /// or whose reservation or prepare checkpoint holds it.
    pub(crate) fn live_task_pilot_run(&self, task_id: &str) -> Result<Option<String>, OrbitError> {
        self.live_pilot_run_for_task(task_id)
    }

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
