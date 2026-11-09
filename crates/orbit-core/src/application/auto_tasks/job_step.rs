//! Auto-task job steps on the owner (ORB-20158;
//! `docs/design/activity-job/specs/auto-task-step.md` §2-§5): freeze an
//! admitted execution, observe one step pass deterministically, admit its one
//! keyed task under the auto-task admission fence (or settle it on a
//! revalidated accepted result its `reuse_when` matches), record its checkpoints and
//! park the run, and recheck a minted child before it starts.
//!
//! Observation writes no task or run state; judging a completion against its
//! declared contract ([`super::completion`]) runs the adapter in its own scratch
//! space on the engine's pass only. Admission requires a durable reservation, so a
//! lost creation reply resolves to the task its creation receipt names
//! instead of a second mint. Job mints never read or write a definition's
//! schedule cursor; they only share its sidecar lock as the fence.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use orbit_automation::condition::{OuterBound, check_job_waits};
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_engine::activity_job::load_job_asset;
use orbit_engine::{AutoTaskAdmission, AutoTaskObservation, AutoTaskReadiness};
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{JobStepInvocation, Task, TaskDecision, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    AUTO_TASK_CHECKPOINT_VERSION, AUTO_TASK_TAG_PREFIX, AutoTaskDefinition, AutoTaskReceipt,
    AutoTaskRunCheckpoint, AutoTaskSchedule, AutoTaskStepRecord, AutoTaskStepStatus, DedupePolicy,
    FrozenAutoTaskTarget, JobRun, JobRunState, JobV2, ParkedWait, PipelineState, RunStateUpdate,
    auto_task_tag,
};
use serde_json::Value;

use super::{completion, cursor_state_path, scheduler};
use crate::OrbitRuntime;
use crate::application::automation::condition::render_placeholders;

/// How long a waiting step parks before the owner observes it again.
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
    let now = Utc::now();
    let mut observation = observe_with(runtime, &step, now)?;
    completion::bind_completion_result(runtime, &step, &mut observation, now);
    Ok(observation)
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
    // Before the step settles, so a crash between the two repeats an idempotent
    // write instead of leaving an accepted step missing from the history.
    if record.status == AutoTaskStepStatus::Done
        && let Some(result) = record
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.result.as_ref())
    {
        completion::record_accepted_result(runtime, &record.auto_task, result)?;
    }
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
    // Immediately before anything new is minted, recorded proofs answer the
    // adapter again: a finding reopened since a reused checkpoint counts here.
    if let Some(hold) =
        completion::prior_proof_hold(runtime, &step, now, completion::ProofDepth::Adapter)
    {
        return Ok(AutoTaskAdmission::Held(hold));
    }
    // Reuse comes before binding or minting. A matched result is revalidated
    // like a bound one; one that no longer applies leaves the step to mint
    // fresh evidence, while an undecidable match holds.
    match reuse_verdict(runtime, &step, now)? {
        ReuseVerdict::NoMatch => {}
        ReuseVerdict::Held(hold) => return Ok(AutoTaskAdmission::Held(hold)),
        ReuseVerdict::Matched(candidate) => {
            match completion::reused_result(runtime, &step, &record, &candidate, now) {
                Ok(result) => {
                    return Ok(AutoTaskAdmission::Reused(Box::new(AutoTaskReceipt {
                        task_id: result.task_id.clone(),
                        task_status: "done".into(),
                        accepted: true,
                        settled_at: result.completed_at,
                        observed_at: now,
                        result: Some(result),
                    })));
                }
                Err(why) => tracing::info!(
                    run_id,
                    step_id,
                    task_id = %candidate.task_id,
                    %why,
                    "reuse_when matched an accepted result that does not apply; minting fresh"
                ),
            }
        }
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
    if let Some(note) = completion::evidence_binding_note(runtime, &step, &definition)? {
        params.description.push_str(&note);
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

/// The bound task's terminal receipt, or its raw status while it is open.
/// A completed task is an accepted completion; archived and rejected tasks
/// are not. The settlement time is the owner's recorded transition.
pub(super) fn bound_task(
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
                    result: None,
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
            result: None,
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

pub(super) fn creation_receipt(history: &[TaskHistoryEntry]) -> Option<&JobStepInvocation> {
    history.iter().find_map(|entry| match &entry.decision {
        Some(TaskDecision::JobStepInvocation(invocation)) => Some(invocation),
        _ => None,
    })
}

/// Resolve the named definition now, with the digest of its file and its
/// resolved body for freezing. `None` when it does not exist.
pub(super) fn resolve_target(
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

pub(super) fn parse_frozen_job(yaml: &str) -> Result<JobV2, OrbitError> {
    load_job_asset(yaml)
        .map(|asset| asset.spec)
        .map_err(|error| OrbitError::InvalidInput(format!("frozen job does not load: {error}")))
}

pub(super) fn serde_name<T: serde::Serialize>(value: &T) -> String {
    orbit_common::protocol::serde_string_label(value).unwrap_or_default()
}

use super::step_observation::{ReuseVerdict, StepContext, observe_with, reuse_verdict};
