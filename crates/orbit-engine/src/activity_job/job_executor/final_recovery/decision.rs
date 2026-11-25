use orbit_types::workflow::{
    FinalRecoveryCheckpointOutcome, FinalRecoveryDecision, JobRunState, PipelineState,
};

use crate::executor::automation::vcs::PUBLISHED_REPLAY_STEPS;
use crate::executor::automation::vcs::git::git_output;

use super::super::*;
use super::types;

/// What a dispatched final recovery answered.
pub(super) struct RecoveryDispatch {
    /// The agent's own answer ([`decision_payload`]).
    pub(super) payload: Value,
    /// The dispatched spec's resolved model [ORB-16768].
    pub(super) model: Option<String>,
    /// The provider observed running the judge, never the agent's claim.
    pub(super) judge: Option<String>,
    /// The resume's `repair_evidence`, held until the engine admits the resume
    /// step and any repair commit ([`persist_repair_evidence`]).
    pub(super) repair_evidence: Option<Value>,
}

/// Dispatch the activity under its configured crew.
pub(super) fn dispatch_final_recovery(
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
    activity: &ResolvedRecoveryActivity,
    input: &Value,
) -> Result<RecoveryDispatch, String> {
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
        let mut payload = decision_payload(&dispatch.output);
        let repair_evidence = payload
            .as_object_mut()
            .and_then(|object| object.remove("repair_evidence"));
        if repair_evidence.is_some() {
            // Only a well-formed, fault-attributed resume may carry evidence. The
            // write itself waits for admission: a resume the engine refuses must
            // not replace the task's summary or attach artifacts.
            let (decision, _fault) = FinalRecoveryDecision::parse_attributed(&payload)
                .map_err(|error| format!("invalid final recovery decision: {error}"))?;
            if !matches!(decision, FinalRecoveryDecision::Resume { .. }) {
                return Err("evidence handoff requires a repair resume decision".into());
            }
        }
        let judge = dispatch
            .output
            .get("provider")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(RecoveryDispatch {
            payload,
            model: recovery_model,
            judge,
            repair_evidence,
        })
    } else {
        Err(dispatch
            .message
            .unwrap_or_else(|| "the activity reported failure without a diagnostic".to_string()))
    }
}

/// Hand a resume's `repair_evidence` to the host for the admitted task. The
/// caller runs this only after the resume's step and any repair commit have
/// passed their gates, and before the decision is recorded: the host writes
/// evidence only to a recovery that is still undecided.
pub(super) fn persist_repair_evidence(
    ctx: &ExecCtx<'_>,
    task: &str,
    workspace: &str,
    evidence: &Value,
) -> Result<(), String> {
    ctx.host
        .persist_final_recovery_evidence(
            &ctx.run_id,
            task,
            std::path::Path::new(workspace),
            evidence,
        )
        .map_err(|error| format!("persist final recovery evidence: {error}"))
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

/// An interrupted final-recovery replay with retained publication history
/// must start at the admitted phase. A replay that checkpointed the way
/// through publication to completion can continue using those checkpoints.
pub(in crate::activity_job::job_executor) fn final_recovery_replay_from(
    job: &JobV2,
    state: &PipelineState,
) -> Option<usize> {
    let checkpoint = state.final_recovery.as_ref()?;
    if checkpoint.repair_commit.is_some() {
        return None;
    }
    let context = checkpoint.resume_context.as_ref()?;
    let from = job
        .steps
        .iter()
        .position(|step| step.id == context.step_id)?;
    let retained_history = PUBLISHED_REPLAY_STEPS.iter().all(|(id, _)| {
        state.pipeline.get(*id).is_some_and(|output| {
            state.step_outputs.iter().any(|(index, recorded)| {
                recorded == output && state.step_states.get(index) == Some(&JobRunState::Success)
            })
        })
    });
    let reached_completion = checkpoint.outcome == Some(FinalRecoveryCheckpointOutcome::Resume)
        && implementation_replay_reached_completion(job, state, from);
    (retained_history && !reached_completion).then_some(from)
}

fn implementation_replay_reached_completion(
    job: &JobV2,
    state: &PipelineState,
    from: usize,
) -> bool {
    // This phase lost its checkpoint when recovery was admitted. Its new
    // success proves the replay started, so the cursor is from the replay
    // rather than the pass that admitted it.
    if PUBLISHED_REPLAY_STEPS
        .iter()
        .any(|(id, _)| *id == job.steps[from].id)
    {
        return false;
    }
    let frontier = job
        .steps
        .iter()
        .enumerate()
        .take(state.next_step_index as usize)
        .take_while(|(index, _)| {
            state.step_states.get(&(*index as u32)) == Some(&JobRunState::Success)
        })
        .count();
    let is_action = |step: &JobV2Step, action: &str| {
        matches!(&step.body, JobV2StepBody::Target(target)
            if matches!(&target.spec, ActivityV2Spec::Deterministic(spec) if spec.action == action))
    };
    let publication = |id: &str, action: &str| {
        job.steps[..frontier]
            .iter()
            .enumerate()
            .skip(from + 1)
            .find(|(index, step)| {
                let index = *index as u32;
                step.id == id
                    && is_action(step, action)
                    && !state.skipped_steps.contains(&index)
                    && state.step_output(index).is_some_and(|output| {
                        !output.is_null() && state.pipeline.get(id) == Some(output)
                    })
            })
            .map(|(index, _)| index)
    };
    let (Some(push), Some(open)) = (
        publication("push", "git_push"),
        publication("pr_open", "pr_open"),
    ) else {
        return false;
    };
    // The completion round itself may be absent after a failure or rewind.
    // Its normal head, task and review guards still authenticate the seeded
    // publication before any completion mutation.
    push < open
        && job
            .steps
            .iter()
            .take(frontier + 1)
            .skip(open + 1)
            .any(|step| is_action(step, "pr_complete"))
}

/// Keep only a complete, exact host publication chain. Incomplete or foreign
/// history gets no continuation and still meets the immutable setup guard.
pub(super) fn preserved_publication_steps(
    job: &JobV2,
    ctx: &ExecCtx<'_>,
    task: &str,
    worktree: &types::RunWorktree,
) -> Vec<u32> {
    use crate::executor::automation::vcs::published_candidate_matches;
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
