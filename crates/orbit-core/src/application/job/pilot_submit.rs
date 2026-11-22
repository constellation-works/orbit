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
use orbit_engine::activity_job::{
    ACTIVITY_REF_PREFIX, V2ActivityCatalog, render_job_step_input_at_admission,
};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{
    ActivityV2Spec, CoreDeterministicAction, JobRunState, JobRunTrigger, JobV2, JobV2Step,
    JobV2StepBody, TASK_PILOT_PIPELINE_JOB,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::automation_port::JobAutomationPort;
use super::{JobRunListParams, PipelineInvokeResult};
use crate::OrbitRuntime;
use crate::application::task::pilot_assessable_status;

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
                        TASK_PILOT_PIPELINE_JOB,
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
        self.show_job_catalog_entry(TASK_PILOT_PIPELINE_JOB)?;
        Ok(())
    }

    /// The live pilot run holding `task_id`: one that names it in its input,
    /// or whose reservation or prepare checkpoint holds it.
    pub(crate) fn live_task_pilot_run(&self, task_id: &str) -> Result<Option<String>, OrbitError> {
        self.live_pilot_run_for_task(task_id)
    }

    fn live_pilot_run_for_task(&self, task_id: &str) -> Result<Option<String>, OrbitError> {
        let named = self.list_job_runs(JobRunListParams {
            job_id: Some(TASK_PILOT_PIPELINE_JOB.to_string()),
            task_id: Some(task_id.to_string()),
            states: vec![
                JobRunState::Pending,
                JobRunState::Running,
                JobRunState::Retrying,
            ],
            limit: Some(PILOT_SCAN_LIMIT),
            ..Default::default()
        })?;
        if let Some(run) = named.into_iter().next() {
            return Ok(Some(run.run_id));
        }
        self.pilot_holder_run(task_id)
    }

    /// Reject caller-provided string `task_ids` (such as CLI `--input
    /// task_ids=...`) when the admitted job definition renders `input.task_ids`
    /// into an invalid `task_ids` value for a `prepare_task_pilot` step.
    ///
    /// Runs against the resolved `JobV2` definition (`Snapshot` for direct YAML
    /// paths, `load_v2_job_asset_by_name` for catalog jobs) before admission
    /// persists any run or reclaims worktrees.
    pub(crate) fn validate_pilot_string_task_ids_input(
        &self,
        job_name: &str,
        spec: &JobV2,
        input: &Value,
    ) -> Result<(), OrbitError> {
        let Some(raw_task_ids) = input.get("task_ids").and_then(Value::as_str) else {
            return Ok(());
        };
        let mut catalog = None;
        let Some((step_id, target_label)) =
            self.first_step_forwarding_task_ids_to_pilot(spec, &spec.steps, input, &mut catalog)?
        else {
            return Ok(());
        };
        Err(OrbitError::InvalidInput(format!(
            "invalid `--input task_ids={raw_task_ids}` for `{job_name}` (step `{step_id}`, \
             {target_label}): `--input key=value` only passes string values, while `{}` \
             requires `task_ids` to be an array of strings. Use `orbit run task-pilot <TASK_ID>...` for \
             explicit tasks, or omit `--input task_ids=...` (or run `orbit run task-pilot`) \
             for automatic discovery; custom jobs can also supply an array `task_ids` in `default_input`",
            CoreDeterministicAction::PrepareTaskPilot.name(),
        )))
    }

    fn first_step_forwarding_task_ids_to_pilot(
        &self,
        spec: &JobV2,
        steps: &[JobV2Step],
        input: &Value,
        catalog: &mut Option<V2ActivityCatalog>,
    ) -> Result<Option<(String, String)>, OrbitError> {
        for step in steps {
            match &step.body {
                JobV2StepBody::Parallel { parallel } => {
                    if let Some(hit) = self.first_step_forwarding_task_ids_to_pilot(
                        spec,
                        &parallel.branches,
                        input,
                        catalog,
                    )? {
                        return Ok(Some(hit));
                    }
                }
                JobV2StepBody::FanOut { fan_out, .. } => {
                    if let Some(hit) = self.first_step_forwarding_task_ids_to_pilot(
                        spec,
                        std::slice::from_ref(fan_out.worker.as_ref()),
                        input,
                        catalog,
                    )? {
                        return Ok(Some(hit));
                    }
                }
                JobV2StepBody::Loop { loop_ } => {
                    if let Some(hit) = self.first_step_forwarding_task_ids_to_pilot(
                        spec,
                        &loop_.steps,
                        input,
                        catalog,
                    )? {
                        return Ok(Some(hit));
                    }
                }
                JobV2StepBody::Target(target) => {
                    if is_prepare_task_pilot_spec(&target.spec)
                        && default_input_renders_invalid_pilot_task_ids(
                            spec.default_input.as_ref(),
                            input,
                            target.default_input.as_ref(),
                            target.input_schema_json.as_ref(),
                        )
                    {
                        return Ok(Some((
                            step.id.clone(),
                            format!(
                                "action `{}`",
                                CoreDeterministicAction::PrepareTaskPilot.name()
                            ),
                        )));
                    }
                }
                JobV2StepBody::TargetRef(reference) => {
                    if !default_input_may_use_caller_task_ids(reference.default_input.as_ref()) {
                        continue;
                    }
                    let Some(activity_name) = reference.target.strip_prefix(ACTIVITY_REF_PREFIX)
                    else {
                        continue;
                    };
                    let loaded = match catalog {
                        Some(loaded) => loaded,
                        None => {
                            let built = self.v2_activity_catalog().map_err(|err| {
                                OrbitError::InvalidInput(format!("build activity catalog: {err}"))
                            })?;
                            catalog.insert(built)
                        }
                    };
                    let resolved_schema = match loaded.get(activity_name) {
                        Some(activity) if is_prepare_task_pilot_spec(&activity.spec) => {
                            Some(Some(&activity.input_schema_json))
                        }
                        Some(_) => None,
                        None if loaded.unavailable_reason(activity_name).is_none()
                            && activity_name
                                == CoreDeterministicAction::PrepareTaskPilot.name() =>
                        {
                            Some(None)
                        }
                        None => None,
                    };
                    if let Some(input_schema) = resolved_schema
                        && default_input_renders_invalid_pilot_task_ids(
                            spec.default_input.as_ref(),
                            input,
                            reference.default_input.as_ref(),
                            input_schema,
                        )
                    {
                        return Ok(Some((
                            step.id.clone(),
                            format!(
                                "target `{}` -> `{}`",
                                reference.target,
                                CoreDeterministicAction::PrepareTaskPilot.name()
                            ),
                        )));
                    }
                }
            }
        }
        Ok(None)
    }
}

/// Non-JSON plain-string probe used beside the caller's actual string `task_ids`
/// to distinguish templates that wrap a string input into a JSON array (such as
/// `["{{ input.task_ids }}"]`) from bare pass-throughs (`{{ input.task_ids }}`)
/// that only happen to parse when a caller passes JSON text via `--input`.
const PILOT_TASK_IDS_STRING_PROBE: &str = "__orbit_pilot_task_id_probe__";

fn is_prepare_task_pilot_spec(spec: &ActivityV2Spec) -> bool {
    matches!(
        spec,
        ActivityV2Spec::Deterministic(det)
            if det.action == CoreDeterministicAction::PrepareTaskPilot.name()
    )
}

fn default_input_may_use_caller_task_ids(default_input: Option<&Value>) -> bool {
    match default_input {
        None => true,
        Some(Value::String(template)) => {
            is_exact_whole_input_template(template)
                || template_references_input_field(template, "task_ids")
        }
        Some(Value::Object(map)) => map
            .get("task_ids")
            .is_some_and(|entry| value_references_input_field(entry, "task_ids")),
        Some(Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_)) => false,
    }
}

fn default_input_renders_invalid_pilot_task_ids(
    job_default_input: Option<&Value>,
    caller_input: &Value,
    step_default_input: Option<&Value>,
    input_schema: Option<&Value>,
) -> bool {
    if !default_input_may_use_caller_task_ids(step_default_input) {
        return false;
    }
    if rendered_step_has_invalid_pilot_task_ids(
        job_default_input,
        caller_input,
        step_default_input,
        input_schema,
    ) {
        return true;
    }
    let mut probe_input = caller_input.clone();
    if let Some(map) = probe_input.as_object_mut() {
        map.insert(
            "task_ids".to_string(),
            Value::String(PILOT_TASK_IDS_STRING_PROBE.to_string()),
        );
    }
    rendered_step_has_invalid_pilot_task_ids(
        job_default_input,
        &probe_input,
        step_default_input,
        input_schema,
    )
}

fn rendered_step_has_invalid_pilot_task_ids(
    job_default_input: Option<&Value>,
    caller_input: &Value,
    step_default_input: Option<&Value>,
    input_schema: Option<&Value>,
) -> bool {
    let scoped_object;
    let candidate = match step_default_input {
        None => {
            scoped_object = json!({
                "task_ids": caller_input.get("task_ids").cloned().unwrap_or(Value::Null)
            });
            Some(&scoped_object)
        }
        Some(Value::String(_)) => step_default_input,
        Some(Value::Object(map)) => {
            let Some(task_ids) = map.get("task_ids") else {
                return false;
            };
            // Render only `task_ids` so unrelated sibling fields referencing
            // prior step outputs do not block admission validation.
            scoped_object = json!({ "task_ids": task_ids });
            Some(&scoped_object)
        }
        Some(Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_)) => return false,
    };
    let Ok(rendered) = render_job_step_input_at_admission(
        job_default_input,
        caller_input,
        candidate,
        input_schema,
    ) else {
        // Templates that depend on runtime-only step/item/iteration outputs
        // cannot be evaluated at admission; defer to execution rather than
        // claiming a type mismatch we could not verify.
        return false;
    };
    !rendered_pilot_input_has_valid_task_ids(&rendered)
}

fn rendered_pilot_input_has_valid_task_ids(rendered: &Value) -> bool {
    let Some(map) = rendered.as_object() else {
        return false;
    };
    match map.get("task_ids") {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.iter().all(Value::is_string),
        Some(Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Object(_)) => false,
    }
}

fn is_exact_whole_input_template(template: &str) -> bool {
    template
        .strip_prefix("{{")
        .and_then(|token| token.strip_suffix("}}"))
        .is_some_and(|token| token.trim() == "input")
}

fn value_references_input_field(value: &Value, field: &str) -> bool {
    match value {
        Value::String(template) => template_references_input_field(template, field),
        Value::Array(items) => items
            .iter()
            .any(|item| value_references_input_field(item, field)),
        Value::Object(map) => map
            .values()
            .any(|entry| value_references_input_field(entry, field)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn template_references_input_field(template: &str, field: &str) -> bool {
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        let after_start = &remaining[start + 2..];
        let Some(end) = after_start.find("}}") else {
            break;
        };
        let token = after_start[..end].trim();
        if token.strip_prefix("input.").is_some_and(|key| key == field) {
            return true;
        }
        remaining = &after_start[end + 2..];
    }
    false
}
