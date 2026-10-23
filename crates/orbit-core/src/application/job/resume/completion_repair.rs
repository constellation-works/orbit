//! The resume an out-of-run auto-fix submits.
//!
//! An owner drain started with `--auto-fix` hands a conflicting or red
//! Orbit pull request whose task has no live run back to the delivery leaf
//! that opened it (`application::job::pr_repair::adoption`). It does so
//! through the ordinary checkpoint resume, rewound so the leaf's completion
//! step runs again: the watcher there admits the repair against the pull
//! request's attempt ledger, dispatches the `workflow.purpose_crews.auto_fix`
//! crew, pushes to the same branch and completes the existing pull request.

use orbit_common::OrbitError;
use orbit_types::workflow::{ActivityV2Spec, JobRunState, JobV2, JobV2StepBody, PipelineState};
use serde_json::Value;

/// Run input key naming the out-of-run auto-fix a resumed leaf performs.
pub(crate) const OUT_OF_RUN_AUTO_FIX_KEY: &str = "out_of_run_auto_fix";

/// The deterministic action of the completion step a repair reruns.
const COMPLETION_ACTION: &str = "pr_complete";

/// Drop the checkpoints of the completion step and every step after it, so
/// the resumed run reruns them. Every step before completion must have
/// finished: a run that stopped earlier has not published what completion
/// watches, and its own resume is an operator's.
///
/// The completion deadline the source recorded is dropped with them. The
/// source ran out of it long ago; the repair's own budget is bounded by the
/// pull request's attempt ledger instead.
pub(super) fn rewind_to_completion(
    definition: &JobV2,
    state: Option<&mut PipelineState>,
) -> Result<(), OrbitError> {
    let completion = definition
        .steps
        .iter()
        .position(|step| match &step.body {
            JobV2StepBody::Target(target) => matches!(
                &target.spec,
                ActivityV2Spec::Deterministic(spec) if spec.action == COMPLETION_ACTION
            ),
            _ => false,
        })
        .ok_or_else(|| {
            OrbitError::JobValidation(format!(
                "the run's job has no `{COMPLETION_ACTION}` step for an auto-fix to rerun"
            ))
        })?;
    let state = state.ok_or_else(|| {
        OrbitError::JobValidation("the run kept no checkpoints for an auto-fix to resume".into())
    })?;
    for (position, step) in definition.steps.iter().enumerate() {
        let index = u32::try_from(position)
            .map_err(|_| OrbitError::JobValidation("the run's job has too many steps".into()))?;
        if position < completion {
            if !matches!(
                state.step_states.get(&index),
                Some(JobRunState::Success | JobRunState::Skipped)
            ) {
                return Err(OrbitError::JobValidation(format!(
                    "step '{}' did not finish before completion; an auto-fix resumes only a run \
                     that reached its completion step",
                    step.id
                )));
            }
            continue;
        }
        state.step_states.remove(&index);
        state.step_outputs.remove(&index);
        state.step_output_pointers.remove(&index);
        state.compound_outputs.remove(&index);
        state.pipeline_patches.remove(&index);
        if let Some(pipeline) = state.pipeline.as_object_mut() {
            pipeline.remove(&step.id);
        }
    }
    state.next_step_index = u32::try_from(completion).unwrap_or(u32::MAX);
    state.completion_deadlines.clear();
    Ok(())
}

/// Grant the resumed run repair authority over its pull request, defer its
/// footprint locks to the first agent that may edit, and record `repair`.
pub(super) fn grant_completion_repair(input: &mut Value, repair: &Value) {
    if !input.is_object() {
        *input = Value::Object(Default::default());
    }
    if let Some(object) = input.as_object_mut() {
        object.insert("auto_fix".into(), Value::Bool(true));
        object.insert("footprint_deferred".into(), Value::Bool(true));
        object.insert(OUT_OF_RUN_AUTO_FIX_KEY.into(), repair.clone());
    }
}
