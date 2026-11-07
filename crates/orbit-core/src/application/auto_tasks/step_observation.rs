//! Read-only evaluation of a frozen auto-task step.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use orbit_automation::condition::{
    CheckedAutoTaskStep, ConditionCarrier, ConditionEvaluator, JobWaitFacts, ResultFacts,
    StepFacts, Truth, auto_task_step_deadline, check_job_waits,
};
use orbit_common::OrbitError;
use orbit_engine::{AutoTaskObservation, AutoTaskReadiness};
use orbit_types::workflow::automation::ConditionScope;
use orbit_types::workflow::{
    AutoTaskAcceptedHistory, AutoTaskCompletionReceipt, AutoTaskDefinition, AutoTaskRunCheckpoint,
    AutoTaskSchedule, AutoTaskStepRecord, AutoTaskStepStatus, JobRunState, JobV2, JobV2Step,
    JobV2StepBody, PipelineState,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::automation::condition::{evaluate_guard, facts};

use super::completion::contract_admission_hold;
use super::job_step::{bound_task, parse_frozen_job, resolve_target};

/// One step of a frozen execution, as its checkpoint records it.
pub(super) struct StepContext {
    pub(super) state: PipelineState,
    pub(super) checkpoint: AutoTaskRunCheckpoint,
    job: JobV2,
    pub(super) checked: CheckedAutoTaskStep,
    /// Every auto-task step of the frozen job, this one included.
    pub(super) steps: Vec<CheckedAutoTaskStep>,
    pub(super) record: Option<AutoTaskStepRecord>,
}

impl StepContext {
    /// A resume can change observation and settlement, but cannot acquire a
    /// different child's binding or reset either the job or step wait clock.
    pub(super) fn preserves_step_obligation(&self, continued: &Self) -> bool {
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

    pub(super) fn load(
        runtime: &OrbitRuntime,
        run_id: &str,
        step_id: &str,
    ) -> Result<Self, OrbitError> {
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
        let steps = check_job_waits(&job, checkpoint.started_at)
            .map_err(|error| OrbitError::InvalidInput(format!("frozen job: {error}")))?
            .steps;
        let checked = steps
            .iter()
            .find(|step| step.id == step_id)
            .cloned()
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
            steps,
            record,
        })
    }

    /// The definition a mint uses: the one this execution froze, or the
    /// current one, which the reservation checkpoint then freezes.
    pub(super) fn minting_definition(
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

    pub(super) fn step_facts(&self, id: &str) -> StepFacts {
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

pub(super) fn observe_with(
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
    let input_digest = step_input_digest(
        name,
        frozen.as_ref().map(|frozen| frozen.digest.as_str()),
        step.checked.require_result.as_deref(),
        step.checkpoint.source_revision.as_deref(),
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
        result_hold: None,
        bound_status,
        deadline: Some(deadline),
        dynamic_expiry,
        expired,
        readiness,
        recheck_at: (now + Duration::seconds(RECHECK_SECONDS)).min(deadline.max(now)),
    })
}

/// The digest of what a step's task is minted for. Its creation receipt keeps
/// this, so evidence can be tied back to one definition and source revision.
pub(super) fn step_input_digest(
    auto_task: &str,
    definition_digest: Option<&str>,
    require_result: Option<&str>,
    source_revision: Option<&str>,
) -> String {
    orbit_automation::delivery::digest(
        json!({
            "auto_task": auto_task,
            "definition": definition_digest,
            "require_result": require_result,
            "source_revision": source_revision,
        })
        .to_string()
        .as_bytes(),
    )
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
    if let Some(hold) = contract_admission_hold(runtime, step, now) {
        return Some(hold);
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
        result: None,
    })
}

/// What a step's `reuse_when` makes of its definition's latest accepted
/// result.
pub(super) enum ReuseVerdict {
    /// No `reuse_when`, no accepted result, or the predicate is false: the
    /// step binds or mints as usual.
    NoMatch,
    /// The predicate holds for this receipt. The caller still revalidates it
    /// against this execution before the step may settle on it.
    Matched(Box<AutoTaskCompletionReceipt>),
    /// The history is unreadable or the predicate is unknown, so whether a
    /// reusable result exists cannot be decided: the step holds instead of
    /// minting.
    Held(String),
}

/// Judge `reuse_when` against the definition's latest accepted result. A
/// definition with none is missing, never unreadable, and an unreadable
/// history never reads as missing.
pub(super) fn reuse_verdict(
    runtime: &OrbitRuntime,
    step: &StepContext,
    now: DateTime<Utc>,
) -> Result<ReuseVerdict, OrbitError> {
    let Some(expression) = step.checked.reuse_when.as_ref() else {
        return Ok(ReuseVerdict::NoMatch);
    };
    let name = &step.checked.auto_task;
    let receipt = match runtime.auto_task_accepted_history(name) {
        AutoTaskAcceptedHistory::Missing => return Ok(ReuseVerdict::NoMatch),
        AutoTaskAcceptedHistory::Unreadable(why) => {
            return Ok(ReuseVerdict::Held(format!(
                "reuse_when cannot judge auto-task `{name}`'s accepted results, which are \
                 unreadable: {why}; nothing is minted"
            )));
        }
        AutoTaskAcceptedHistory::Latest(receipt) => receipt,
    };
    let mut facts = wait_facts(runtime, step, now)?;
    facts.result = Some(ResultFacts {
        status: receipt.status.as_str().to_string(),
        revision: Some(receipt.revision.clone()),
        completed_at: receipt.completed_at,
        contract: receipt.contract.clone(),
        definition_digest: receipt.definition_digest.clone(),
        task_id: receipt.task_id.clone(),
        run_id: Some(receipt.run_id.clone()),
    });
    Ok(
        match ConditionEvaluator::for_job_wait(&facts).truth(expression, None) {
            Truth::True => ReuseVerdict::Matched(receipt),
            Truth::False => ReuseVerdict::NoMatch,
            Truth::Unknown(why) => ReuseVerdict::Held(format!(
                "reuse_when `{}` is unknown: {why}; nothing is minted",
                expression.source
            )),
        },
    )
}

/// The observation cadence while a frozen step waits.
const RECHECK_SECONDS: i64 = 60;
