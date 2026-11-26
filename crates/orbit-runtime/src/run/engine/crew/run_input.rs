use crate::RuntimeKernel;
use crate::run::run_input::{non_empty, singular_task_id_from_input};
use orbit_common::OrbitError;
use orbit_types::identity::{
    CrewAssignment, all_agent_families, infer_agent_family_from_model, resolve_crew,
};
use orbit_types::task::{CrewSource, Task, TaskComplexity, is_valid_orb_task_id};
use orbit_types::workflow::activity_job::{
    ActivityV2, ActivityV2Spec, JobV2, JobV2Step, JobV2StepBody,
};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

/// The shipped task-pilot job. Its recorded run crew is the agent that
/// assesses tasks, not the crew a task is assigned to.
pub const TASK_PILOT_JOB: &str = "task_pilot_pipeline";

/// Run-input key under which a pulled claimed leaf carries the owner's
/// snapshot of its task. Written when the follower creates the leaf
/// (`orbit-store` `LocalPullMutation::CreateLeaf`) from the claim receipt.
const CLAIMED_TASK_INPUT_KEY: &str = "claimed_task";

impl RuntimeKernel {
    /// Whether the job at `yaml_path` can dispatch an agent. A definition that
    /// does not load or resolve counts as agent-bearing: the run's own load
    /// reports that failure, and the crew is recorded as before.
    pub fn job_definition_dispatches_agent(&self, yaml_path: &Path) -> bool {
        let resolved = (|| {
            let yaml = std::fs::read_to_string(yaml_path).ok()?;
            let mut job = orbit_engine::activity_job::load_job_asset(&yaml).ok()?.spec;
            let catalog = self.v2_activity_catalog().ok()?;
            orbit_engine::resolve_job_catalog_refs_for_execution(&mut job, &catalog).ok()?;
            Some(job)
        })();
        resolved.is_none_or(|job| job_dispatches_agent(&job))
    }

    pub fn implementer_identity_for_activity_input(
        &self,
        input: &Value,
    ) -> Result<(Option<String>, Option<String>), OrbitError> {
        let task = self.task_id_from_run_input(input)?;
        let input_run_id = input
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        // An explicit evidence run may differ from the task's binding (for
        // example, a recovery run). It still must not resolve a foreign task's
        // own run ID against a colliding local record [ORB-14635].
        if let Some(task) = &task
            && !self.task_run_is_local(task)
            && input_run_id.is_none_or(|run_id| task.job_run_id.as_deref() == Some(run_id))
        {
            return Ok((None, None));
        }
        let task_run_id = task.as_ref().and_then(|task| task.job_run_id.as_deref());
        let Some(run_id) = input_run_id.or(task_run_id) else {
            return Ok((None, None));
        };
        let Some(run) = self.stores().jobs().get_job_run(run_id)? else {
            return Ok((None, None));
        };

        if let Some(crew_name) = run
            .resolved_crew
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            && let Ok(crew) = resolve_crew(crew_name, self.base().settings().crews())
            && let Some(family) = family_from_assignment(&crew.assignment)
        {
            return Ok((Some(family.clone()), Some(family)));
        }

        if let Some(family) = run
            .crew_model
            .as_deref()
            .and_then(infer_agent_family_from_model)
        {
            return Ok((Some(family.clone()), Some(family)));
        }

        Ok((None, None))
    }

    fn task_id_from_run_input(&self, input: &Value) -> Result<Option<Task>, OrbitError> {
        if let Some(task_id) = singular_task_id_from_input(input)
            && is_valid_orb_task_id(task_id)
            && let Some(task) = self.stores().tasks().get_task(task_id)?
        {
            return Ok(Some(task));
        }

        for key in ["task_id", "taskId", "id"] {
            let Some(task_id) = input.get(key).and_then(Value::as_str).and_then(non_empty) else {
                continue;
            };
            if is_valid_orb_task_id(task_id)
                && let Some(task) = self.stores().tasks().get_task(task_id)?
            {
                return Ok(Some(task));
            }
        }
        Ok(None)
    }
}

/// The owner's view of a claimed task, as far as crew resolution needs it.
#[derive(Debug, Deserialize)]
pub struct ClaimedTaskSnapshot {
    pub id: String,
    #[serde(default)]
    crew: Option<String>,
    /// How `crew` was assigned on the owner [ORB-17773]; absent on a leaf
    /// created before it was carried, which reads as not automatic.
    #[serde(default)]
    pub crew_source: Option<CrewSource>,
    #[serde(default)]
    pub complexity: Option<TaskComplexity>,
}

impl ClaimedTaskSnapshot {
    /// The owner's crew for the task, when it names one.
    pub fn crew(&self) -> Option<&str> {
        self.crew.as_deref().and_then(non_empty)
    }
}

/// Whether a run's task bundle may name tasks whose stored crews differ.
///
/// The runtime cannot read automation claims, so the application layer that
/// starts a run decides. Run input alone is never enough: any caller can
/// shape it [ORB-16860].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleCrews {
    /// Every named task must agree on its crew; a mix fails closed.
    Unanimous,
    /// The run is the live task-pilot preparation attempt's own admitted run.
    /// It keeps the first task's crew, and prepare partitions the rest by
    /// crew before any pilot worker runs [ORB-16839].
    PreparationPartitions,
    /// The task-auto pipeline's backlog action emits one task per gate bundle
    /// before any task worker starts.
    AutoDispatchSingletons,
    /// A verified drain pilot whose tasks' crews changed after the drain
    /// selected a homogeneous bundle. It keeps the first task's current
    /// crew at run start, and prepare re-partitions by live `task.crew`
    /// before any pilot worker runs [ORB-21839].
    DrainPartitions,
}

/// Collect `task_id` / `task.id` / `task_ids` entries. Generic `id` is left to
/// `RuntimeKernel::task_id_from_run_input` so a non-task `id` cannot pollute
/// mixed-crew detection.
pub fn task_ids_for_crew_resolution(input: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    let mut push = |raw: Option<&str>| {
        if let Some(id) = raw.and_then(non_empty)
            && !ids.iter().any(|existing| existing == id)
        {
            ids.push(id.to_string());
        }
    };
    push(input.get("task_id").and_then(Value::as_str));
    push(
        input
            .get("task")
            .and_then(|task| task.get("id"))
            .and_then(Value::as_str),
    );
    if let Some(items) = input.get("task_ids").and_then(Value::as_array) {
        for item in items {
            push(item.as_str());
        }
    }
    ids
}

/// Read the claimed-task snapshot off a run input. Absent or `null` means the
/// run is not a pulled leaf; a malformed one fails closed rather than quietly
/// resolving the follower's default crew.
pub fn claimed_task_from_input(input: &Value) -> Result<Option<ClaimedTaskSnapshot>, OrbitError> {
    match input.get(CLAIMED_TASK_INPUT_KEY) {
        None | Some(Value::Null) => Ok(None),
        Some(raw) => serde_json::from_value(raw.clone())
            .map(Some)
            .map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "`{CLAIMED_TASK_INPUT_KEY}` must be an object with a task `id` and optional \
                     `crew`, `crew_source` and `complexity`: {error}"
                ))
            }),
    }
}

pub fn normalized_task_crew(crew: Option<&str>) -> Option<String> {
    crew.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub fn mixed_bundle_crew_error(left: Option<&str>, right: Option<&str>) -> String {
    format!(
        "task bundle mixes crews {} and {}; split the bundle or assign one crew instead of inheriting workflow.default_crew",
        format_bundle_crew(left),
        format_bundle_crew(right),
    )
}

fn format_bundle_crew(crew: Option<&str>) -> String {
    match crew {
        Some(name) => format!("`{name}`"),
        None => "the workspace default (no task.crew)".to_string(),
    }
}

fn family_from_assignment(assignment: &CrewAssignment) -> Option<String> {
    let provider = assignment.provider.trim().to_ascii_lowercase();
    if all_agent_families()
        .iter()
        .any(|family| *family == provider)
    {
        return Some(provider);
    }

    infer_agent_family_from_model(&assignment.model)
}

/// Whether a resolved job can dispatch an agent-loop activity, as a step at any
/// nesting depth or as a recovery, failure or final-recovery hook [ORB-13016]. A reference the
/// catalog has not resolved counts as one, so only a definition proven
/// agent-free skips the run's crew record.
pub(crate) fn job_dispatches_agent(job: &JobV2) -> bool {
    hook_dispatches_agent(
        job.recovery_activity.as_deref(),
        job.resolved_recovery_activity.as_ref(),
    ) || hook_dispatches_agent(
        job.failure_activity.as_deref(),
        job.resolved_failure_activity.as_ref(),
    ) || hook_dispatches_agent(
        job.final_recovery_activity.as_deref(),
        job.resolved_final_recovery_activity.as_ref(),
    ) || job.steps.iter().any(step_dispatches_agent)
}

fn step_dispatches_agent(step: &JobV2Step) -> bool {
    if hook_dispatches_agent(
        step.recovery_activity.as_deref(),
        step.resolved_recovery_activity.as_ref(),
    ) {
        return true;
    }
    match &step.body {
        JobV2StepBody::Target(target) => matches!(target.spec, ActivityV2Spec::AgentLoop(_)),
        JobV2StepBody::TargetRef(_) => true,
        JobV2StepBody::Parallel { parallel } => parallel.branches.iter().any(step_dispatches_agent),
        JobV2StepBody::FanOut { fan_out, .. } => step_dispatches_agent(&fan_out.worker),
        JobV2StepBody::Loop { loop_ } => loop_.steps.iter().any(step_dispatches_agent),
    }
}

fn hook_dispatches_agent(name: Option<&str>, resolved: Option<&ActivityV2>) -> bool {
    match resolved {
        Some(activity) => matches!(activity.spec, ActivityV2Spec::AgentLoop(_)),
        None => name.is_some(),
    }
}

/// `metadata.name` of the job at `yaml_path`, when the file loads.
pub fn loaded_job_name(yaml_path: &Path) -> Option<String> {
    let yaml = std::fs::read_to_string(yaml_path).ok()?;
    orbit_engine::activity_job::load_job_asset(&yaml)
        .ok()
        .map(|job| job.name)
}

/// The run input's explicit `crew`, when it names one.
pub fn run_input_crew_override(input: &Value) -> Option<&str> {
    input
        .get("crew")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}
