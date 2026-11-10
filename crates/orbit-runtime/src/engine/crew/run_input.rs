use crate::RuntimeKernel;
use crate::run_input::{non_empty, singular_task_id_from_input};
use orbit_common::OrbitError;
use orbit_types::identity::{
    Crew, CrewAssignment, all_agent_families, infer_agent_family_from_model, resolve_crew,
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
const TASK_PILOT_JOB: &str = "task_pilot_pipeline";

impl RuntimeKernel {
    pub fn resolve_crew_for_run_input(
        &self,
        input: &Value,
        bundle: BundleCrews,
    ) -> Result<Crew, OrbitError> {
        let cli_override = run_input_crew_override(input);
        let claimed = claimed_task_from_input(input)?;
        let task_crew = self.task_crew_from_run_input(input, claimed.as_ref(), bundle)?;
        self.resolve_crew_for_task(cli_override, task_crew.as_deref())
            .map_err(|error| {
                // A crew the owner chose but this host cannot run fails the
                // leaf by name. Falling back to `default_crew` would ship the
                // owner's task on a provider nobody selected for it.
                match claimed.as_ref().zip(task_crew.as_deref()) {
                    Some((snapshot, crew))
                        if cli_override.is_none() && snapshot.crew() == Some(crew) =>
                    {
                        OrbitError::InvalidInput(format!(
                            "claimed task {} has crew `{crew}` on its owner, which this host \
                             cannot run: {error}; configure and enable `{crew}` here or reassign \
                             the task on the owner (a claimed leaf never falls back to \
                             workflow.default_crew)",
                            snapshot.id,
                        ))
                    }
                    _ => error,
                }
            })
    }

    /// Resolve a run's crew at start, persisting it only when the job at
    /// `yaml_path` can dispatch an agent [ORB-13016].
    ///
    /// Every job still resolves, so a crew misconfiguration (an unknown name,
    /// no crew selected) fails the run at start as it always has. A job made
    /// only of deterministic activities records no crew: no model does its
    /// work, and a crew drawn for it would read as an LLM that never ran. For
    /// the same reason a disabled crew does not refuse it: on a host with no
    /// enabled default crew, retention and reap jobs must still run.
    ///
    /// `bundle` is the caller's authenticated answer to whether this run may
    /// name tasks whose crews differ; see [`BundleCrews`].
    ///
    /// A task-pilot job does not take a human task crew as its agent crew.
    /// Prepare excludes those tasks, and the run keeps an agent crew instead
    /// of failing at start. An explicit human crew on the run input is still
    /// refused, and every other job still refuses a human task crew.
    pub fn record_run_crew_for_job(
        &self,
        run_id: &str,
        input: &Value,
        yaml_path: &Path,
        bundle: BundleCrews,
    ) -> Result<(), OrbitError> {
        let dispatches_agent = self.job_definition_dispatches_agent(yaml_path);
        if dispatches_agent && loaded_job_name(yaml_path).as_deref() == Some(TASK_PILOT_JOB) {
            self.record_task_pilot_run_crew(run_id, input, bundle)?;
        } else if dispatches_agent {
            self.record_run_crew_from_input(run_id, input, bundle)?;
        } else {
            let claimed = claimed_task_from_input(input)?;
            let task_crew = self.task_crew_from_run_input(input, claimed.as_ref(), bundle)?;
            self.lookup_crew_for_task(run_input_crew_override(input), task_crew.as_deref())?;
        }
        Ok(())
    }

    /// The task-pilot run's agent crew. A human task crew is operator work:
    /// it is not this run's crew, and it does not fail the run. Prepare
    /// excludes those tasks. Any other agreed crew resolves as it does for
    /// every job, including the claimed-leaf error.
    fn record_task_pilot_run_crew(
        &self,
        run_id: &str,
        input: &Value,
        bundle: BundleCrews,
    ) -> Result<Crew, OrbitError> {
        let claimed = claimed_task_from_input(input)?;
        let assigned = self.task_crew_from_run_input(input, claimed.as_ref(), bundle)?;
        let crew = if self.is_human_crew(assigned.as_deref()) {
            let task_crew = self.agent_task_crew_from_run_input(input, claimed.as_ref(), bundle)?;
            self.resolve_crew_for_task(run_input_crew_override(input), task_crew.as_deref())?
        } else {
            self.resolve_crew_for_run_input(input, bundle)?
        };
        tracing::info!(
            run_id,
            resolved_crew = %crew.name,
            crew_model = %crew.assignment.model,
            "crew resolved for run",
        );
        self.stores().jobs().record_job_run_crew(run_id, &crew)?;
        Ok(crew)
    }

    /// Whether the job at `yaml_path` can dispatch an agent. A definition that
    /// does not load or resolve counts as agent-bearing: the run's own load
    /// reports that failure, and the crew is recorded as before.
    fn job_definition_dispatches_agent(&self, yaml_path: &Path) -> bool {
        let resolved = (|| {
            let yaml = std::fs::read_to_string(yaml_path).ok()?;
            let mut job = orbit_engine::activity_job::load_job_asset(&yaml).ok()?.spec;
            let catalog = self.v2_activity_catalog().ok()?;
            orbit_engine::resolve_job_catalog_refs_for_execution(&mut job, &catalog).ok()?;
            Some(job)
        })();
        resolved.is_none_or(|job| job_dispatches_agent(&job))
    }

    fn record_run_crew_from_input(
        &self,
        run_id: &str,
        input: &Value,
        bundle: BundleCrews,
    ) -> Result<Crew, OrbitError> {
        let crew = self.resolve_crew_for_run_input(input, bundle)?;
        tracing::info!(
            run_id,
            resolved_crew = %crew.name,
            crew_model = %crew.assignment.model,
            "crew resolved for run",
        );
        self.stores().jobs().record_job_run_crew(run_id, &crew)?;
        Ok(crew)
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
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok((None, None));
        };

        if let Some(crew_name) = run
            .resolved_crew
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            && let Ok(crew) = resolve_crew(crew_name, self.context.settings().crews())
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

    /// Unanimous `task.crew` across every task named by the run/activity
    /// input. A task the input carries a `claimed_task` snapshot for reads its
    /// crew from that snapshot: a pulled task lives in the owner's store, and a
    /// same-id local record, if any, is not it. Other ids read the local store;
    /// missing fixture ids are skipped so fake implementer ids do not fail
    /// resolution. Distinct crews — including a mix of set and unset — fail
    /// closed instead of inheriting `workflow.default_crew`. Authenticated
    /// preparation, drain-pilot, and auto-dispatch runs are exceptions: each
    /// keeps the first task's crew, then partitions tasks before agent work.
    fn task_crew_from_run_input(
        &self,
        input: &Value,
        claimed: Option<&ClaimedTaskSnapshot>,
        bundle: BundleCrews,
    ) -> Result<Option<String>, OrbitError> {
        self.agreed_task_crew(input, claimed, bundle, false)
    }

    /// [`Self::task_crew_from_run_input`], ignoring human task crews.
    ///
    /// A task pilot uses this only after the unfiltered agreement is itself a
    /// human crew. Human crews then neither become the run's agent crew nor
    /// count as a disagreement, so an all-human bundle resolves with no task
    /// crew and a partition whose first crew is human keeps the first agent
    /// crew. A mixed bundle that already failed closed stays failed: this
    /// walk does not replace that result.
    fn agent_task_crew_from_run_input(
        &self,
        input: &Value,
        claimed: Option<&ClaimedTaskSnapshot>,
        bundle: BundleCrews,
    ) -> Result<Option<String>, OrbitError> {
        self.agreed_task_crew(input, claimed, bundle, true)
    }

    fn agreed_task_crew(
        &self,
        input: &Value,
        claimed: Option<&ClaimedTaskSnapshot>,
        bundle: BundleCrews,
        skip_human_task_crews: bool,
    ) -> Result<Option<String>, OrbitError> {
        let mut agreed: Option<Option<String>> = None;
        for task_id in task_ids_for_crew_resolution(input) {
            if !is_valid_orb_task_id(&task_id) {
                continue;
            }
            let crew = match claimed.filter(|snapshot| snapshot.id == task_id) {
                // [ORB-17773] An automatic assignment is re-drawn under
                // this follower's own usage pressure; `None` keeps the
                // owner's crew exactly as the snapshot carries it.
                Some(snapshot) => self
                    .claimed_leaf_redrawn_crew(
                        input,
                        &snapshot.id,
                        snapshot.crew(),
                        snapshot.crew_source.as_ref(),
                        snapshot.complexity,
                    )
                    .or_else(|| snapshot.crew().map(ToOwned::to_owned)),
                None => match self.stores().tasks().get_task(&task_id)? {
                    Some(task) => normalized_task_crew(task.crew.as_deref()),
                    None => continue,
                },
            };
            // A human crew is operator work. When this walk is choosing a
            // task pilot's agent crew, that assignment is not a vote.
            if skip_human_task_crews && self.is_human_crew(crew.as_deref()) {
                continue;
            }
            match &agreed {
                None => agreed = Some(crew),
                Some(existing) if existing == &crew => {}
                // The live preparation attempt's run keeps the first task's
                // crew while prepare splits its claim by crew before pilot
                // workers run. A verified drain pilot similarly partitions by
                // live crew. The auto-dispatch run also keeps the first task's
                // crew; its backlog action emits singleton gate
                // bundles before any task worker starts. Every other bundle,
                // including one whose input merely carries a claim, fails
                // closed below [ORB-16860].
                Some(existing)
                    if matches!(
                        bundle,
                        BundleCrews::PreparationPartitions
                            | BundleCrews::AutoDispatchSingletons
                            | BundleCrews::DrainPartitions
                    ) =>
                {
                    return Ok(existing.clone());
                }
                Some(existing) => {
                    return Err(OrbitError::InvalidInput(mixed_bundle_crew_error(
                        existing.as_deref(),
                        crew.as_deref(),
                    )));
                }
            }
        }
        Ok(agreed.flatten())
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

/// Collect `task_id` / `task.id` / `task_ids` entries. Generic `id` is left to
/// [`RuntimeKernel::task_id_from_run_input`] so a non-task `id` cannot pollute
/// mixed-crew detection.
fn task_ids_for_crew_resolution(input: &Value) -> Vec<String> {
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

/// Run-input key under which a pulled claimed leaf carries the owner's
/// snapshot of its task. Written when the follower creates the leaf
/// (`orbit-store` `LocalPullMutation::CreateLeaf`) from the claim receipt.
const CLAIMED_TASK_INPUT_KEY: &str = "claimed_task";

/// The owner's view of a claimed task, as far as crew resolution needs it.
#[derive(Debug, Deserialize)]
struct ClaimedTaskSnapshot {
    id: String,
    #[serde(default)]
    crew: Option<String>,
    /// How `crew` was assigned on the owner [ORB-17773]; absent on a leaf
    /// created before it was carried, which reads as not automatic.
    #[serde(default)]
    crew_source: Option<CrewSource>,
    #[serde(default)]
    complexity: Option<TaskComplexity>,
}

impl ClaimedTaskSnapshot {
    fn crew(&self) -> Option<&str> {
        self.crew.as_deref().and_then(non_empty)
    }
}

/// Read the claimed-task snapshot off a run input. Absent or `null` means the
/// run is not a pulled leaf; a malformed one fails closed rather than quietly
/// resolving the follower's default crew.
fn claimed_task_from_input(input: &Value) -> Result<Option<ClaimedTaskSnapshot>, OrbitError> {
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

/// Whether a run's task bundle may name tasks whose stored crews differ.
///
/// The runtime cannot read automation claims, so the application layer that
/// starts a run decides and hands the answer down. Run input alone is never
/// enough: any caller can shape it [ORB-16860].
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

fn mixed_bundle_crew_error(left: Option<&str>, right: Option<&str>) -> String {
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
fn loaded_job_name(yaml_path: &Path) -> Option<String> {
    let yaml = std::fs::read_to_string(yaml_path).ok()?;
    orbit_engine::activity_job::load_job_asset(&yaml)
        .ok()
        .map(|job| job.name)
}

/// The run input's explicit `crew`, when it names one.
fn run_input_crew_override(input: &Value) -> Option<&str> {
    input
        .get("crew")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}
