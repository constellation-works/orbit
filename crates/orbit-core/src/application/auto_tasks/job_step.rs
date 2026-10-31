//! Auto-task job steps on the owner (ORB-20158;
//! `docs/design/activity-job/specs/auto-task-step.md` §2-§5): freeze an
//! admitted execution, observe one step pass deterministically, admit its one
//! keyed task under the auto-task admission fence, record its checkpoints and
//! park the run, and recheck a minted child before it starts.
//!
//! Observation writes nothing. Admission requires a durable reservation, so a
//! lost creation reply resolves to the task its creation receipt names
//! instead of a second mint. Job mints never read or write a definition's
//! schedule cursor; they only share its sidecar lock as the fence.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use orbit_automation::condition::{
    CheckedAutoTaskStep, ConditionCarrier, ConditionEvaluator, JobWaitFacts, OuterBound, StepFacts,
    Truth, auto_task_step_deadline, check_job_waits,
};
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_engine::activity_job::load_job_asset;
use orbit_engine::{AutoTaskAdmission, AutoTaskObservation, AutoTaskReadiness};
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{JobStepInvocation, Task, TaskDecision, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::automation::ConditionScope;
use orbit_types::workflow::{
    AUTO_TASK_CHECKPOINT_VERSION, AUTO_TASK_TAG_PREFIX, AutoTaskDefinition, AutoTaskReceipt,
    AutoTaskRunCheckpoint, AutoTaskSchedule, AutoTaskStepRecord, AutoTaskStepStatus, DedupePolicy,
    FrozenAutoTaskTarget, JobRun, JobRunState, JobV2, JobV2Step, JobV2StepBody, ParkedWait,
    PipelineState, RunStateUpdate, auto_task_tag,
};
use serde_json::{Value, json};

use super::{cursor_state_path, scheduler};
use crate::OrbitRuntime;
use crate::application::automation::condition::{evaluate_guard, facts, render_placeholders};

/// How long a waiting step parks before the owner observes it again.
const RECHECK_SECONDS: i64 = 60;
/// Clock skew a creation-receipt lookup tolerates before a reservation.
const RECEIPT_LOOKBACK_SECONDS: i64 = 300;

/// Freeze an admitted execution of a job with auto-task steps: the definition
/// it runs, its admission time, its outer deadline and, when a step requires
/// `release-readiness/v1`, the pinned integration revision. A run seeded from
/// a resume already carries its source's checkpoint and keeps it unchanged.
pub(crate) fn freeze_auto_task_run(
    runtime: &OrbitRuntime,
    run_id: &str,
    yaml: &str,
    now: DateTime<Utc>,
) -> Result<(), OrbitError> {
    let job = parse_frozen_job(yaml)?;
    let checked = check_job_waits(&job, now)
        .map_err(|error| OrbitError::InvalidInput(format!("auto-task job: {error}")))?;
    let Some(expiry) = checked.expiry else {
        return Ok(());
    };
    let expires_at = match expiry.bound {
        OuterBound::At(at) => at,
        OuterBound::AfterStart(duration) => now + duration,
    };
    let source_revision = if checked.pinned_revision {
        let branch = runtime.workflow_base_branch().to_string();
        let revision =
            crate::application::automation::source::Source::new(&runtime.paths().repo_root)
                .verify_branch(&branch)
                .map_err(|error| {
                    OrbitError::InvalidInput(format!(
                        "a release-readiness/v1 step pins the integration branch `{branch}` at \
                 admission, which did not resolve: {error}; nothing was submitted"
                    ))
                })?;
        Some(revision.commit)
    } else {
        None
    };
    let checkpoint = AutoTaskRunCheckpoint {
        version: AUTO_TASK_CHECKPOINT_VERSION,
        frozen_job: yaml.to_string(),
        frozen_targets: Default::default(),
        started_at: now,
        source_revision,
        expires_at,
        steps: Default::default(),
        branches: Default::default(),
        parked: None,
    };
    let mut freeze = |_: JobRunState, state: &mut PipelineState| {
        state
            .checkpoint_write_supported()
            .map_err(OrbitError::InvalidInput)?;
        if state.auto_task.is_none() {
            state.auto_task = Some(checkpoint.clone());
        }
        Ok(())
    };
    let jobs = runtime.stores().jobs();
    match jobs.update_run_state(run_id, &mut freeze)? {
        RunStateUpdate::Updated => return Ok(()),
        RunStateUpdate::NotFound => {
            return Err(OrbitError::not_found(
                NotFoundKind::JobRun,
                run_id.to_string(),
            ));
        }
        RunStateUpdate::NoState => {}
    }
    // A keyed or child admission is not seeded until its worker runs: the
    // checkpoint is its first state, unless another writer got there first.
    let run = runtime
        .get_job_run_backend(run_id)?
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
    let mut state = PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        run.input.clone().unwrap_or(Value::Null),
    );
    state.auto_task = Some(checkpoint.clone());
    if !jobs.initialize_run_state(run_id, &state)? {
        update_existing_state(runtime, run_id, &mut freeze)?;
    }
    Ok(())
}

/// The owner's observation of one step pass. Writes nothing.
pub(crate) fn observe_step(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_id: &str,
    auto_task: &str,
) -> Result<AutoTaskObservation, OrbitError> {
    let step = StepContext::load(runtime, run_id, step_id)?;
    if step.checked.auto_task != auto_task {
        return Err(OrbitError::InvalidInput(format!(
            "step `{step_id}` targets auto-task `{}` in its frozen definition, not `{auto_task}`",
            step.checked.auto_task
        )));
    }
    observe_with(runtime, &step, Utc::now())
}

/// Record a step's record in the run's checkpoint, freezing its target the
/// first time this execution resolves it. Refuses a finished run, a
/// checkpoint written by a newer binary, and a settled record moving back.
pub(crate) fn checkpoint_step_record(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_id: &str,
    record: &AutoTaskStepRecord,
) -> Result<(), OrbitError> {
    let frozen = runtime
        .read_run_state(run_id)?
        .and_then(|state| state.auto_task)
        .filter(|checkpoint| !checkpoint.frozen_targets.contains_key(&record.auto_task))
        .map(|_| resolve_target(runtime, &record.auto_task, Utc::now()))
        .transpose()?
        .flatten()
        .map(|(_, frozen)| frozen);
    update_existing_state(runtime, run_id, &mut |run_state, state| {
        let checkpoint = live_checkpoint(run_id, run_state, state)?;
        if let Some(frozen) = &frozen {
            checkpoint
                .frozen_targets
                .entry(record.auto_task.clone())
                .or_insert_with(|| frozen.clone());
        }
        let mut next = record.clone();
        if let Some(existing) = checkpoint.steps.get(step_id) {
            if existing.status.is_settled() && *existing != next {
                return Err(OrbitError::InvalidInput(format!(
                    "step `{step_id}` already settled {}; its record does not change",
                    existing.status.as_str()
                )));
            }
            // Activation is persisted once, and a binding is never lost.
            next.activated_at = existing.activated_at;
            if next.task_id.is_none() {
                next.task_id.clone_from(&existing.task_id);
                next.minted = existing.minted;
            }
            if next.reserved_at.is_none() {
                next.reserved_at = existing.reserved_at;
            }
        }
        checkpoint.steps.insert(step_id.to_string(), next);
        Ok(())
    })
}

/// Record a succeeded `parallel:` branch's output for replay.
pub(crate) fn checkpoint_branch(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_id: &str,
    branch_id: &str,
    output: &Value,
) -> Result<(), OrbitError> {
    update_existing_state(runtime, run_id, &mut |run_state, state| {
        live_checkpoint(run_id, run_state, state)?
            .branches
            .entry(step_id.to_string())
            .or_default()
            .insert(branch_id.to_string(), output.clone());
        Ok(())
    })
}

/// Park a running execution until `park.wake_at`.
pub(crate) fn park_run(
    runtime: &OrbitRuntime,
    run_id: &str,
    park: &ParkedWait,
) -> Result<(), OrbitError> {
    update_existing_state(runtime, run_id, &mut |run_state, state| {
        if run_state != JobRunState::Running {
            return Err(OrbitError::InvalidInput(format!(
                "run {run_id} is {run_state}; only a running run parks"
            )));
        }
        live_checkpoint(run_id, run_state, state)?.parked = Some(park.clone());
        Ok(())
    })
}

/// Resolve the step's durable reservation to one task under the auto-task
/// admission fence: the task its creation receipt names, a compatible open
/// instance, or one keyed mint after rechecking expiry, the definition and
/// readiness.
pub(crate) fn admit_step(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_id: &str,
) -> Result<AutoTaskAdmission, OrbitError> {
    runtime.ensure_coordination_task_write_permitted()?;
    // The sidecar lock the scheduler and manual mints hold. Its cursor is
    // neither read nor written: a job mint consumes no schedule slot.
    let fence = cursor_state_path(&runtime.paths().state_dir);
    with_exclusive_file_lock(&fence, "auto-task cursor", || {
        admit_step_fenced(runtime, run_id, step_id)
    })
}

fn admit_step_fenced(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_id: &str,
) -> Result<AutoTaskAdmission, OrbitError> {
    let step = StepContext::load(runtime, run_id, step_id)?;
    let record = step.record.clone().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "step `{step_id}` has no durable record; nothing was admitted"
        ))
    })?;
    if let Some(task_id) = record.task_id {
        return Ok(AutoTaskAdmission::Bound {
            task_id,
            minted: record.minted,
        });
    }
    let Some(reserved_at) = record.reserved_at else {
        return Err(OrbitError::InvalidInput(format!(
            "step `{step_id}` has no durable reservation; nothing was admitted"
        )));
    };
    // A reply lost after the create: the receipt names the task.
    if let Some(task) = minted_for(runtime, &record, reserved_at)? {
        return Ok(AutoTaskAdmission::Bound {
            task_id: task.id,
            minted: true,
        });
    }
    let now = Utc::now();
    let observation = observe_with(runtime, &step, now)?;
    if let Some(reason) = observation.expired {
        return Ok(AutoTaskAdmission::Expired(reason));
    }
    if let Some(reason) = observation.definition_hold {
        return Ok(AutoTaskAdmission::Held(reason));
    }
    if let AutoTaskReadiness::Waiting(reason) = observation.readiness {
        return Ok(AutoTaskAdmission::Waiting(reason));
    }
    let Some(definition) = step.minting_definition(runtime, now)? else {
        return Ok(AutoTaskAdmission::Held(format!(
            "auto-task `{}` did not resolve",
            record.auto_task
        )));
    };
    if definition.dedupe == DedupePolicy::SkipIfOpen
        && let Some(open) = scheduler::open_auto_task_instances(runtime, &definition.name)?
            .into_iter()
            .next()
    {
        return match open_instance_mismatch(runtime, &open, &record, &step)? {
            None => Ok(AutoTaskAdmission::Bound {
                task_id: open,
                minted: false,
            }),
            Some(why) => Ok(AutoTaskAdmission::Held(format!(
                "open instance {open} of auto-task `{}` is incompatible ({why}); no task was \
                 created",
                definition.name
            ))),
        };
    }
    runtime.validate_required_tools(&definition.template.required_tools)?;
    let mut params = scheduler::mint_params(runtime, &definition);
    if matches!(definition.schedule, AutoTaskSchedule::Condition { .. }) {
        params.title = render_placeholders(&params.title, None);
        params.description = render_placeholders(&params.description, None);
    }
    params.creation_decision = Some(TaskDecision::JobStepInvocation(JobStepInvocation {
        action_key: record.action_key.clone(),
        input_digest: record.input_digest.clone(),
        auto_task: record.auto_task.clone(),
        run_id: run_id.to_string(),
        step_id: step_id.to_string(),
    }));
    let task = runtime.add_task(params)?;
    Ok(AutoTaskAdmission::Bound {
        task_id: task.id,
        minted: true,
    })
}

/// Why a task an auto-task step minted may not start now: its lineage ended, its
/// step settled or expired, its definition holds, or the step's readiness no
/// longer holds. `None` for every other task: binding existing work never
/// restricts it. An unreadable parent holds rather than admits. History is
/// read only for a task carrying an auto-task tag, as every mint does.
pub(crate) fn step_bound_admission_hold(
    runtime: &OrbitRuntime,
    task: &Task,
    history: &dyn Fn() -> Result<Vec<TaskHistoryEntry>, OrbitError>,
) -> Option<String> {
    if !task
        .tags
        .iter()
        .any(|tag| tag.starts_with(AUTO_TASK_TAG_PREFIX))
    {
        return None;
    }
    let history = match history() {
        Ok(history) => history,
        Err(error) => {
            return Some(format!(
                "job step admission: its creation receipt is unreadable: {error}"
            ));
        }
    };
    let invocation = creation_receipt(&history)?;
    let hold = |why: String| {
        Some(format!(
            "job step admission: run {} step `{}`: {why}",
            invocation.run_id, invocation.step_id
        ))
    };
    let run = match runtime.get_job_run_backend(&invocation.run_id) {
        Ok(Some(run)) => run,
        Ok(None) => return hold("its run no longer exists".into()),
        Err(error) => return hold(format!("its run is unreadable: {error}")),
    };
    let mut step = match StepContext::load(runtime, &invocation.run_id, &invocation.step_id) {
        Ok(step) => step,
        Err(error) => return hold(format!("its checkpoint is unreadable: {error}")),
    };
    if step.record.as_ref().map(|record| record.task_id.as_deref()) != Some(Some(&task.id)) {
        return hold(format!("the step is not bound to {}", task.id));
    }
    if !step.record.as_ref().is_some_and(|record| {
        record.action_key == invocation.action_key
            && record.input_digest == invocation.input_digest
            && record.auto_task == invocation.auto_task
    }) {
        return hold("the checkpoint does not match its creation receipt".into());
    }
    if run.state.is_terminal() {
        let continuation = match live_invocation_continuation(runtime, &run) {
            Ok(Some(continuation)) => continuation,
            Ok(None) => return hold(format!("its run ended {}; no live continuation", run.state)),
            Err(error) => return hold(format!("its continuation is unreadable: {error}")),
        };
        let continued = match StepContext::load(runtime, &continuation.run_id, &invocation.step_id)
        {
            Ok(continued) => continued,
            Err(error) => {
                return hold(format!(
                    "its continuation checkpoint is unreadable: {error}"
                ));
            }
        };
        if !step.preserves_step_obligation(&continued) {
            return hold("its continuation does not preserve the original step obligation".into());
        }
        step = continued;
    }
    if let Some(record) = step
        .record
        .as_ref()
        .filter(|record| record.status.is_settled())
    {
        return hold(format!(
            "the step already settled {}",
            record.status.as_str()
        ));
    }
    let observation = match observe_with(runtime, &step, Utc::now()) {
        Ok(observation) => observation,
        Err(error) => return hold(format!("its readiness is unreadable: {error}")),
    };
    if let Some(reason) = observation.expired {
        return hold(reason);
    }
    if let Some(reason) = observation.definition_hold {
        return hold(reason);
    }
    if let AutoTaskReadiness::Waiting(reason) = observation.readiness {
        return hold(format!("wait_until no longer holds: {reason}"));
    }
    None
}

/// Resolve only explicit descendants, never an unrelated execution of this
/// job. Admission chooses no winner when stored lineage is ambiguous or
/// unreadable. Read active rows without hydrating historical step outputs.
fn live_invocation_continuation(
    runtime: &OrbitRuntime,
    source: &JobRun,
) -> Result<Option<JobRun>, OrbitError> {
    const SCAN_LIMIT: usize = 500;
    const MAX_HOPS: usize = 64;
    let candidates = runtime
        .stores()
        .jobs()
        .list_job_runs_filtered(&JobRunQuery {
            job_id: Some(source.job_id.clone()),
            states: vec![
                JobRunState::Pending,
                JobRunState::Running,
                JobRunState::Retrying,
            ],
            limit: Some(SCAN_LIMIT),
            include_steps: false,
            input_keys: Some(&[]),
            ..Default::default()
        })?;
    if candidates.len() == SCAN_LIMIT {
        return Err(OrbitError::InvalidInput(
            "live continuation scan is incomplete".into(),
        ));
    }
    let mut continuation = None;
    for candidate in candidates {
        let mut cursor = candidate.clone();
        let mut seen = BTreeSet::new();
        loop {
            if seen.len() >= MAX_HOPS || !seen.insert(cursor.run_id.clone()) {
                return Err(OrbitError::InvalidInput(
                    "retry lineage is cyclic or too deep".into(),
                ));
            }
            if cursor.job_id != source.job_id {
                return Err(OrbitError::InvalidInput(
                    "retry lineage changes job identity".into(),
                ));
            }
            if cursor.run_id == source.run_id {
                if continuation.replace(candidate).is_some() {
                    return Err(OrbitError::InvalidInput(
                        "multiple live continuations".into(),
                    ));
                }
                break;
            }
            let Some(parent) = cursor.retry_source_run_id else {
                break;
            };
            cursor = runtime.get_job_run_backend(&parent)?.ok_or_else(|| {
                OrbitError::InvalidInput("retry lineage has a missing parent".into())
            })?;
        }
    }
    Ok(continuation)
}

/// One step of a frozen execution, as its checkpoint records it.
struct StepContext {
    state: PipelineState,
    checkpoint: AutoTaskRunCheckpoint,
    job: JobV2,
    checked: CheckedAutoTaskStep,
    record: Option<AutoTaskStepRecord>,
}

impl StepContext {
    /// A resume can change observation and settlement, but cannot acquire a
    /// different child's binding or reset either the job or step wait clock.
    fn preserves_step_obligation(&self, continued: &Self) -> bool {
        let Some((original, next)) = self.record.as_ref().zip(continued.record.as_ref()) else {
            return false;
        };
        !original.status.is_settled()
            && self.checkpoint.frozen_job == continued.checkpoint.frozen_job
            && self.checkpoint.started_at == continued.checkpoint.started_at
            && self.checkpoint.expires_at == continued.checkpoint.expires_at
            && self.checkpoint.source_revision == continued.checkpoint.source_revision
            && self.checkpoint.frozen_targets.get(&original.auto_task)
                == continued.checkpoint.frozen_targets.get(&original.auto_task)
            && original.auto_task == next.auto_task
            && original.action_key == next.action_key
            && original.input_digest == next.input_digest
            && original.task_id == next.task_id
            && original.activated_at == next.activated_at
            && original.reserved_at == next.reserved_at
            && original.minted == next.minted
    }

    fn load(runtime: &OrbitRuntime, run_id: &str, step_id: &str) -> Result<Self, OrbitError> {
        let state = runtime.read_run_state(run_id)?.ok_or_else(|| {
            OrbitError::InvalidInput(format!("run {run_id} has no state; nothing was admitted"))
        })?;
        state
            .checkpoint_write_supported()
            .map_err(OrbitError::InvalidInput)?;
        let checkpoint = state.auto_task.clone().ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "run {run_id} was not admitted as an auto-task execution: it has no frozen \
                 checkpoint, so nothing waits or is minted"
            ))
        })?;
        let job = parse_frozen_job(&checkpoint.frozen_job)?;
        let checked = check_job_waits(&job, checkpoint.started_at)
            .map_err(|error| OrbitError::InvalidInput(format!("frozen job: {error}")))?
            .steps
            .into_iter()
            .find(|step| step.id == step_id)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "step `{step_id}` is not an auto-task step of run {run_id}'s frozen job"
                ))
            })?;
        let record = checkpoint.steps.get(step_id).cloned();
        Ok(Self {
            state,
            checkpoint,
            job,
            checked,
            record,
        })
    }

    /// The definition a mint uses: the one this execution froze, or the
    /// current one, which the reservation checkpoint then freezes.
    fn minting_definition(
        &self,
        runtime: &OrbitRuntime,
        now: DateTime<Utc>,
    ) -> Result<Option<AutoTaskDefinition>, OrbitError> {
        match self.checkpoint.frozen_targets.get(&self.checked.auto_task) {
            Some(frozen) => serde_json::from_str(&frozen.source)
                .map(Some)
                .map_err(|error| {
                    OrbitError::InvalidInput(format!(
                        "frozen auto-task `{}` does not parse: {error}",
                        self.checked.auto_task
                    ))
                }),
            None => Ok(resolve_target(runtime, &self.checked.auto_task, now)?
                .map(|(definition, _)| definition)),
        }
    }

    fn step_facts(&self, id: &str) -> StepFacts {
        if let Some(record) = self.checkpoint.steps.get(id) {
            return StepFacts {
                status: record.status.as_str().to_string(),
                end_time: record.settled_at,
                task_id: record.task_id.clone(),
            };
        }
        // An ordinary step: a completed one reads `done`, a `when: false`
        // skip stays `skipped`, anything else is still `waiting`.
        StepFacts {
            status: self.ordinary_step_status(id).to_string(),
            end_time: None,
            task_id: None,
        }
    }

    fn ordinary_step_status(&self, id: &str) -> &'static str {
        let Some(located) = locate_step(&self.job.steps, id) else {
            return "waiting";
        };
        // A step inside a loop or fan_out has no single durable outcome.
        if located.dynamic {
            return "waiting";
        }
        let Some(parallel_id) = located.enclosing_parallel else {
            // Top-level: the run state is keyed by this step's own index.
            let index = located.top_index as u32;
            if self.state.step_states.get(&index) != Some(&JobRunState::Success) {
                return "waiting";
            }
            let skipped = located.step.when.is_some()
                && self
                    .state
                    .step_outputs
                    .get(&index)
                    .is_none_or(Value::is_null);
            return if skipped { "skipped" } else { "done" };
        };
        // A parallel branch: its enclosing top-level block must have
        // completed, so a checkpoint under a still-live block (a cross-branch
        // read the checker refuses) cannot masquerade as done. The branch's
        // own outcome is the checkpoint a succeeded branch recorded; only
        // succeeded branches are recorded, and a `when:` skip records null.
        if self.state.step_states.get(&(located.top_index as u32)) != Some(&JobRunState::Success) {
            return "waiting";
        }
        let Some(output) = self
            .checkpoint
            .branches
            .get(parallel_id)
            .and_then(|branches| branches.get(id))
        else {
            return "waiting";
        };
        if located.step.when.is_some() && output.is_null() {
            "skipped"
        } else {
            "done"
        }
    }
}

/// One step's static position in a job, mirroring the checker's walk in
/// `orbit-automation`: its top-level ancestor, its nearest enclosing
/// `parallel` block, and whether a loop or fan_out encloses it.
struct LocatedStep<'a> {
    step: &'a JobV2Step,
    top_index: usize,
    enclosing_parallel: Option<&'a str>,
    dynamic: bool,
}

fn locate_step<'a>(steps: &'a [JobV2Step], id: &str) -> Option<LocatedStep<'a>> {
    fn walk<'a>(
        steps: &'a [JobV2Step],
        id: &str,
        top_index: Option<usize>,
        enclosing_parallel: Option<&'a str>,
        dynamic: bool,
        found: &mut Option<LocatedStep<'a>>,
    ) {
        for (position, step) in steps.iter().enumerate() {
            if found.is_some() {
                return;
            }
            let top_index = top_index.unwrap_or(position);
            if step.id == id {
                *found = Some(LocatedStep {
                    step,
                    top_index,
                    enclosing_parallel,
                    dynamic,
                });
                return;
            }
            match &step.body {
                JobV2StepBody::Parallel { parallel } => {
                    for branch in &parallel.branches {
                        walk(
                            std::slice::from_ref(branch),
                            id,
                            Some(top_index),
                            Some(step.id.as_str()),
                            dynamic,
                            found,
                        );
                    }
                }
                JobV2StepBody::FanOut { fan_out, .. } => walk(
                    std::slice::from_ref(&*fan_out.worker),
                    id,
                    Some(top_index),
                    enclosing_parallel,
                    true,
                    found,
                ),
                JobV2StepBody::Loop { loop_ } => walk(
                    &loop_.steps,
                    id,
                    Some(top_index),
                    enclosing_parallel,
                    true,
                    found,
                ),
                JobV2StepBody::TargetRef(_) | JobV2StepBody::Target(_) => {}
            }
        }
    }

    let mut found = None;
    walk(steps, id, None, None, false, &mut found);
    found
}

fn observe_with(
    runtime: &OrbitRuntime,
    step: &StepContext,
    now: DateTime<Utc>,
) -> Result<AutoTaskObservation, OrbitError> {
    let record = step.record.as_ref();
    let name = &step.checked.auto_task;
    let frozen = match step.checkpoint.frozen_targets.get(name) {
        Some(frozen) => Some(frozen.clone()),
        None => resolve_target(runtime, name, now)?.map(|(_, frozen)| frozen),
    };
    let input_digest = orbit_automation::delivery::digest(
        json!({
            "auto_task": name,
            "definition": frozen.as_ref().map(|frozen| &frozen.digest),
            "require_result": step.checked.require_result,
            "source_revision": step.checkpoint.source_revision,
        })
        .to_string()
        .as_bytes(),
    );
    let bound = record.and_then(|record| record.task_id.as_deref());
    let (receipt, bound_status) = match bound {
        Some(task_id) => bound_task(runtime, task_id, now)?,
        None => (None, None),
    };
    let facts = wait_facts(runtime, step, now)?;
    let evaluator = ConditionEvaluator::for_job_wait(&facts);
    let outer_deadline = step.checkpoint.expires_at;
    let step_deadline = step.checked.expire_when.as_ref().and_then(|expression| {
        auto_task_step_deadline(
            expression,
            step.checkpoint.started_at,
            facts.wait_started_at,
        )
    });
    let deadline = step_deadline.map_or(outer_deadline, |at| at.min(outer_deadline));
    let step_expired = step
        .checked
        .expire_when
        .as_ref()
        .filter(|expression| evaluator.truth(expression, None) == Truth::True);
    let dynamic_expiry = step_expired.is_some() && step_deadline.is_none();
    let expired = if now >= outer_deadline {
        Some(format!(
            "the job expired at {} (spec.expire_when)",
            outer_deadline.to_rfc3339()
        ))
    } else {
        step_expired.map(|expression| format!("expire_when `{}` is true", expression.source))
    };
    let readiness = match &step.checked.wait_until {
        None => AutoTaskReadiness::Ready,
        Some(expression) => match evaluator.truth(expression, None) {
            Truth::True => AutoTaskReadiness::Ready,
            Truth::False => AutoTaskReadiness::Waiting(format!("`{}` is false", expression.source)),
            Truth::Unknown(why) => {
                AutoTaskReadiness::Waiting(format!("`{}` is unknown: {why}", expression.source))
            }
        },
    };
    Ok(AutoTaskObservation {
        now,
        record: record.cloned(),
        action_key: format!("{}:{}", step.state.run_id, step.checked.id),
        input_digest,
        definition_hold: definition_hold(runtime, step, frozen.is_some(), now),
        receipt,
        bound_status,
        deadline: Some(deadline),
        dynamic_expiry,
        expired,
        readiness,
        recheck_at: (now + Duration::seconds(RECHECK_SECONDS)).min(deadline.max(now)),
    })
}

/// Why the referenced definition holds admission now, judged by its current
/// file: authority and enablement are current even though the execution
/// mints from its frozen copy.
fn definition_hold(
    runtime: &OrbitRuntime,
    step: &StepContext,
    resolved: bool,
    now: DateTime<Utc>,
) -> Option<String> {
    let name = &step.checked.auto_task;
    if let Some(contract) = &step.checked.require_result {
        return Some(format!(
            "require_result `{contract}`: this Orbit has no validated evidence adapter for the \
             contract, so the step's result would be unavailable; nothing is minted"
        ));
    }
    let definition = match runtime.auto_task_show(name) {
        Ok(Some(definition)) => definition,
        Ok(None) => return Some(format!("auto-task `{name}` is missing")),
        Err(error) => return Some(format!("auto-task `{name}` is unreadable: {error}")),
    };
    if !resolved {
        return Some(format!("auto-task `{name}` did not resolve"));
    }
    if !definition.enabled {
        return Some(format!("auto-task `{name}` is disabled"));
    }
    let (source, clause) = match &definition.schedule {
        AutoTaskSchedule::Condition { condition }
            if condition.scope != ConditionScope::Workspace =>
        {
            return Some(format!(
                "auto-task `{name}` has a {} condition, which has no bindable subject in a job \
                 step",
                condition.scope.as_str()
            ));
        }
        AutoTaskSchedule::Condition { condition } => (condition.when.as_str(), "condition.when"),
        schedule => (
            schedule.guard().and_then(|guard| guard.when.as_deref())?,
            "schedule.when",
        ),
    };
    evaluate_guard(runtime, source, clause, ConditionCarrier::AutoTask, now)
        .describe()
        .map(|why| format!("auto-task `{name}` admission guard: {why}"))
}

fn wait_facts(
    runtime: &OrbitRuntime,
    step: &StepContext,
    now: DateTime<Utc>,
) -> Result<JobWaitFacts, OrbitError> {
    let record = step.record.as_ref();
    // Only this step's own bound task leaves its board.
    let own_tasks: BTreeSet<String> = record
        .and_then(|record| record.task_id.clone())
        .into_iter()
        .collect();
    let gathered = facts::gather(
        runtime,
        &step.checked.reads,
        now,
        &facts::FactInputs {
            own_tasks,
            self_facts: Default::default(),
        },
    )?;
    let board = gathered.facts.board;
    let board_empty = match (board.backlog, board.in_progress, board.review) {
        (Some(backlog), Some(in_progress), Some(review)) => {
            Some(backlog + in_progress + review == 0)
        }
        _ => None,
    };
    Ok(JobWaitFacts {
        now,
        board,
        board_empty,
        tasks: gathered.facts.tasks,
        job_started_at: step.checkpoint.started_at,
        job_source_revision: step.checkpoint.source_revision.clone(),
        wait_started_at: record.map_or(now, |record| record.activated_at),
        status: record
            .map_or(AutoTaskStepStatus::Waiting, |record| record.status)
            .as_str()
            .to_string(),
        task_id: record.and_then(|record| record.task_id.clone()),
        prev: step.checked.prev.as_deref().map(|id| step.step_facts(id)),
        steps: step
            .checked
            .reads
            .steps
            .iter()
            .map(|id| (id.clone(), step.step_facts(id)))
            .collect(),
    })
}

/// The bound task's terminal receipt, or its raw status while it is open.
/// A completed task is an accepted completion; archived and rejected tasks
/// are not. The settlement time is the owner's recorded transition.
fn bound_task(
    runtime: &OrbitRuntime,
    task_id: &str,
    now: DateTime<Utc>,
) -> Result<(Option<AutoTaskReceipt>, Option<String>), OrbitError> {
    let task = match runtime.get_task(task_id) {
        Ok(task) => task,
        Err(OrbitError::NotFound { .. }) => {
            return Ok((
                Some(AutoTaskReceipt {
                    task_id: task_id.to_string(),
                    task_status: "missing".into(),
                    accepted: false,
                    settled_at: now,
                    observed_at: now,
                }),
                None,
            ));
        }
        Err(error) => return Err(error),
    };
    let status = serde_name(&task.status);
    if !matches!(
        task.status,
        TaskStatus::Done | TaskStatus::Archived | TaskStatus::Rejected
    ) {
        return Ok((None, Some(status)));
    }
    let settled_at = runtime
        .get_task_history(task_id)?
        .iter()
        .rev()
        .find(|entry| entry.to_status == Some(task.status))
        .map_or(task.updated_at, |entry| entry.at);
    Ok((
        Some(AutoTaskReceipt {
            task_id: task_id.to_string(),
            task_status: status,
            accepted: task.status == TaskStatus::Done,
            settled_at,
            observed_at: now,
        }),
        None,
    ))
}

/// The task a reservation's mint created, found by its creation receipt.
fn minted_for(
    runtime: &OrbitRuntime,
    record: &AutoTaskStepRecord,
    reserved_at: DateTime<Utc>,
) -> Result<Option<Task>, OrbitError> {
    let since = reserved_at - Duration::seconds(RECEIPT_LOOKBACK_SECONDS);
    for task in runtime.list_tasks_by_tags(&[auto_task_tag(&record.auto_task)])? {
        if task.created_at < since {
            continue;
        }
        if creation_receipt(&runtime.get_task_history(&task.id)?)
            .is_some_and(|invocation| invocation.action_key == record.action_key)
        {
            return Ok(Some(task));
        }
    }
    Ok(None)
}

/// Why an open instance cannot discharge this step, or `None` when it can.
/// One minted for a job step must carry the same input digest; any other open
/// instance binds only when the step pins neither a source revision nor a
/// result contract it could fail to cover.
fn open_instance_mismatch(
    runtime: &OrbitRuntime,
    task_id: &str,
    record: &AutoTaskStepRecord,
    step: &StepContext,
) -> Result<Option<String>, OrbitError> {
    let history = runtime.get_task_history(task_id)?;
    Ok(match creation_receipt(&history) {
        Some(invocation) if invocation.input_digest == record.input_digest => None,
        Some(_) => Some("minted for different definition inputs".into()),
        None if step.checkpoint.source_revision.is_some() => {
            Some("it was not minted for this job's pinned revision".into())
        }
        None => None,
    })
}

fn creation_receipt(history: &[TaskHistoryEntry]) -> Option<&JobStepInvocation> {
    history.iter().find_map(|entry| match &entry.decision {
        Some(TaskDecision::JobStepInvocation(invocation)) => Some(invocation),
        _ => None,
    })
}

/// Resolve the named definition now, with the digest of its file and its
/// resolved body for freezing. `None` when it does not exist.
fn resolve_target(
    runtime: &OrbitRuntime,
    name: &str,
    now: DateTime<Utc>,
) -> Result<Option<(AutoTaskDefinition, FrozenAutoTaskTarget)>, OrbitError> {
    let Some((_, path)) = runtime.auto_task_read_source(name)? else {
        return Ok(None);
    };
    let bytes = std::fs::read(&path)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
    let Some(definition) = runtime.auto_task_show(name)? else {
        return Ok(None);
    };
    let source = serde_json::to_string(&definition)
        .map_err(|error| OrbitError::Execution(format!("freeze auto-task `{name}`: {error}")))?;
    Ok(Some((
        definition,
        FrozenAutoTaskTarget {
            digest: orbit_automation::delivery::digest(&bytes),
            source,
            resolved_at: now,
        },
    )))
}

/// Update a run's stored state in one transaction; a run or state that is
/// not there is an error, so no checkpoint write is silently dropped.
fn update_existing_state(
    runtime: &OrbitRuntime,
    run_id: &str,
    update: &mut dyn FnMut(JobRunState, &mut PipelineState) -> Result<(), OrbitError>,
) -> Result<(), OrbitError> {
    match runtime.stores().jobs().update_run_state(run_id, update)? {
        RunStateUpdate::Updated => Ok(()),
        RunStateUpdate::NotFound => Err(OrbitError::not_found(
            NotFoundKind::JobRun,
            run_id.to_string(),
        )),
        RunStateUpdate::NoState => Err(OrbitError::InvalidInput(format!(
            "run {run_id} has no state; its auto-task checkpoint was not written"
        ))),
    }
}

/// The checkpoint of a live run this binary may rewrite.
fn live_checkpoint<'a>(
    run_id: &str,
    run_state: JobRunState,
    state: &'a mut PipelineState,
) -> Result<&'a mut AutoTaskRunCheckpoint, OrbitError> {
    if run_state.is_terminal() {
        return Err(OrbitError::InvalidInput(format!(
            "run {run_id} is {run_state}; its auto-task checkpoint is final"
        )));
    }
    state
        .checkpoint_write_supported()
        .map_err(OrbitError::InvalidInput)?;
    state.auto_task.as_mut().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "run {run_id} was not admitted as an auto-task execution"
        ))
    })
}

fn parse_frozen_job(yaml: &str) -> Result<JobV2, OrbitError> {
    load_job_asset(yaml)
        .map(|asset| asset.spec)
        .map_err(|error| OrbitError::InvalidInput(format!("frozen job does not load: {error}")))
}

fn serde_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}
