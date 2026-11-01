use orbit_types::workflow::FinalRecoveryDecision;

use crate::executor::automation::vcs::git::git_output;

use super::super::*;
use super::types;

/// Dispatch the activity under its configured crew, returning its output and
/// the dispatched spec's resolved model [ORB-16768].
pub(super) fn dispatch_final_recovery(
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
    activity: &ResolvedRecoveryActivity,
    input: &Value,
) -> Result<(Value, Option<String>), String> {
    let mut spec = crew_overridden_recovery_spec(activity, ctx, input)
        .map_err(|error| format!("crew: {error}"))?
        .unwrap_or_else(|| activity.spec.clone());
    let recovery_model = match &spec {
        ActivityV2Spec::AgentLoop(agent) => agent.model.clone(),
        ActivityV2Spec::Deterministic(_) => None,
    };
    if let ActivityV2Spec::AgentLoop(agent) = &mut spec {
        agent.instruction.push_str(&format!(
            "\n\nFor this failure, resume only from one of these step ids. The first \
             admission step and later steps are excluded.\nAllowed resume step ids: {}",
            input["allowed_resume_step_ids"]
        ));
    }
    let dispatch = dispatch_v2_activity_without_run_id_injection(V2DispatchInput {
        activity_name: &activity.name,
        spec: &spec,
        fs_profile: step_fs_profile(step),
        input: input.clone(),
        audit: ctx.audit.clone(),
        run_id: &ctx.run_id,
        host: Some(ctx.host),
    })
    .map_err(|error| error.to_string())?;
    persist_dispatch_invocation(ctx, &activity.name, input, &dispatch);
    if dispatch.success {
        Ok((dispatch.output, recovery_model))
    } else {
        Err(dispatch
            .message
            .unwrap_or_else(|| "the activity reported failure without a diagnostic".to_string()))
    }
}

/// The agent's own answer inside an activity output.
///
/// An agent activity's output is its envelope `result` merged with Orbit's
/// invocation metadata, and `response_result_fields` names the agent's keys.
/// The decision contract refuses unknown fields, so only those keys are
/// parsed; an output without the list (a deterministic activity) is the
/// answer as it stands.
pub(super) fn decision_payload(output: &Value) -> Value {
    let (Some(fields), Some(object)) = (
        output
            .get("response_result_fields")
            .and_then(Value::as_array),
        output.as_object(),
    ) else {
        return output.clone();
    };
    Value::Object(
        fields
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|key| {
                object
                    .get(key)
                    .map(|value| (key.to_string(), value.clone()))
            })
            .collect(),
    )
}

/// Keep a `resume` when it names the failed step or an earlier step of its
/// phase. A later step downgrades to the failed step, preserving the intent
/// to continue without skipping the failed contract. Missing and admission
/// steps still become `escalate`.
///
/// Phases are top-level steps — the executor's checkpoint and resume unit —
/// so a step nested in a block resumes by naming its block. The job's first
/// step admits the run's tasks and creates its worktree; it is a phase of its
/// own, never rerun to repair a later failure.
pub(super) fn admit_resume_step(
    job: &JobV2,
    failed_index: usize,
    decision: FinalRecoveryDecision,
) -> (FinalRecoveryDecision, Option<usize>, Option<String>) {
    let FinalRecoveryDecision::Resume { step_id, rationale } = &decision else {
        return (decision, None, None);
    };
    let step_id = step_id.trim();
    let target = job.steps.iter().position(|step| step.id == step_id);
    match target {
        Some(index) if (1..=failed_index).contains(&index) => (decision, Some(index), None),
        Some(index) if index > failed_index => {
            let failed = &job.steps[failed_index].id;
            let downgrade = format!(
                "downgraded resume from `{step_id}` to failed step `{failed}` to rerun its contract"
            );
            (
                FinalRecoveryDecision::Resume {
                    step_id: failed.clone(),
                    rationale: rationale.clone(),
                },
                Some(failed_index),
                Some(downgrade),
            )
        }
        _ => {
            let failed = &job.steps[failed_index].id;
            (
                FinalRecoveryDecision::Escalate {
                    diagnosis: format!(
                        "final recovery asked to resume from `{step_id}`, which is neither the \
                         failed step `{failed}` nor an earlier step of its phase"
                    ),
                    human_action: format!(
                        "Repair the worktree and resume the run from `{failed}` or an earlier \
                         step after the first, or move the task by hand."
                    ),
                    summary: None,
                },
                None,
                None,
            )
        }
    }
}

/// Keep only a complete, exact host publication chain. Incomplete or foreign
/// history gets no continuation and still meets the immutable setup guard.
pub(super) fn preserved_publication_steps(
    job: &JobV2,
    ctx: &ExecCtx<'_>,
    task: &str,
    worktree: &types::RunWorktree,
) -> Vec<u32> {
    use crate::executor::automation::vcs::{PUBLISHED_REPLAY_STEPS, published_candidate_matches};
    let Some(state) = ctx.host.read_run_state(&ctx.run_id).ok().flatten() else {
        return Vec::new();
    };
    let root = std::path::Path::new(&worktree.workspace_path);
    let Some(pin) = &worktree.base_sha else {
        return Vec::new();
    };
    let Ok(head) = git_output(root, &["rev-parse", "--verify", "HEAD^{commit}"]) else {
        return Vec::new();
    };
    if !published_candidate_matches(&state, &ctx.run_id, task, root, pin, &head).unwrap_or(false) {
        return Vec::new();
    }
    PUBLISHED_REPLAY_STEPS
        .into_iter()
        .map(|(id, action)| {
            let (index, step) = job
                .steps
                .iter()
                .enumerate()
                .find(|(_, step)| step.id == id)?;
            if !matches!(&step.body, JobV2StepBody::Target(target)
            if matches!(&target.spec, ActivityV2Spec::Deterministic(spec) if spec.action == action))
                || state.step_output(index as u32) != state.pipeline.get(id)
                || state.step_states.get(&(index as u32))
                    != Some(&orbit_types::workflow::JobRunState::Success)
            {
                return None;
            }
            Some(index as u32)
        })
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}
