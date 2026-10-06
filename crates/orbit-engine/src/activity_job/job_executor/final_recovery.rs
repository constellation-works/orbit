//! [ORB-13907] The job-level final recovery hook.
//!
//! When a top-level step fails after its own retry and step recovery are
//! spent — or skipped, for error kinds that bypass step recovery — a job that
//! declares `final_recovery_activity` gets one last automated look before its
//! `failure_activity`. The activity returns a typed
//! [`FinalRecoveryDecision`]; the engine owns `resume`, and the host applies
//! every other decision through the deterministic applier.
//!
//! The hook runs at most once per run. The host records its admission in run
//! state before dispatch, and the executor also refuses a second invocation
//! in the same process and in any run resumed from that state. A run resumed
//! from state that already recorded a decision other than `resume` hands that
//! decision to the host again instead of dispatching. The host applies a
//! decision idempotently, so a crash anywhere in the first application
//! converges on the same outcome.

use orbit_types::workflow::{FINAL_RECOVERY_CREWS_KEY, FinalRecoveryDecision, PipelineState};

use super::*;
use crate::context::{
    FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
    FinalRecoveryApplied,
};

/// What the executor does after the hook.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FinalRecoveryVerdict {
    /// The hook did not run; today's failure path follows.
    Skipped,
    /// Rerun the pipeline from this top-level step index.
    Resume(usize),
    /// The task was settled; the run fails without its `failure_activity`.
    Settled,
    /// The task is parked for a human; the run's `failure_activity` follows.
    Escalated,
}

impl FinalRecoveryVerdict {
    /// Whether the job's terminal `failure_activity` still runs.
    pub(super) fn runs_failure_activity(&self) -> bool {
        matches!(self, Self::Skipped | Self::Escalated)
    }
}

/// Whether this run may still invoke its final recovery.
#[derive(Debug)]
pub(super) enum FinalRecoveryBudget {
    /// Not invoked yet.
    Available,
    /// A run this one resumed decided this and did not resume; the host
    /// applies it again, without a second dispatch.
    Replay(FinalRecoveryDecision),
    /// Invoked already, by this run or one it resumed.
    Spent,
}

impl FinalRecoveryBudget {
    pub(super) fn from_resume(resume: Option<&PipelineState>) -> Self {
        match resume.and_then(|state| state.final_recovery.as_ref()) {
            None => Self::Available,
            Some(checkpoint) => match &checkpoint.decision {
                Some(decision) if !matches!(decision, FinalRecoveryDecision::Resume { .. }) => {
                    Self::Replay(decision.clone())
                }
                _ => Self::Spent,
            },
        }
    }
}

/// The failed run's worktree, as its earliest step reported it.
struct RunWorktree {
    workspace_path: String,
    base_ref: Option<String>,
    base_sha: Option<String>,
}

/// Run the job's final recovery for the failure of top-level step
/// `step_index`, at most once per run. `budget` is the executor's own guard;
/// it is spent as soon as the host is asked, whatever it answers.
pub(super) fn attempt_final_recovery(
    job: &JobV2,
    ctx: &ExecCtx<'_>,
    step_index: usize,
    error_message: &str,
    budget: &mut FinalRecoveryBudget,
) -> FinalRecoveryVerdict {
    let Some(activity) = ctx.final_recovery_activity.as_ref() else {
        return FinalRecoveryVerdict::Skipped;
    };
    let step = &job.steps[step_index];
    let skip = |reason: &str| {
        emit_final_recovery_event(ctx, step, activity, "skipped", None, Some(reason));
        FinalRecoveryVerdict::Skipped
    };
    if matches!(budget, FinalRecoveryBudget::Spent) {
        return skip("final recovery already ran for this run");
    }
    // [ORB-13987] No decision about the task fixes a host that lacks a tool;
    // the failure handoff keeps the candidate for a resume instead.
    if orbit_types::workflow::is_validation_environment_failure(None, Some(error_message)) {
        return skip(
            "required validation lacked a tool in its environment; the candidate was not judged",
        );
    }
    // [ORB-14258] Nor fix a required command the base fails the same way; the
    // failure handoff holds the task until the base moves.
    if orbit_types::workflow::is_baseline_red_failure(None, Some(error_message)) {
        return skip(
            "required validation fails on the base exactly as on the candidate; the candidate \
             did not cause it",
        );
    }
    // [ORB-14149] Nor does any decision give the provider's model capacity;
    // the candidate stays in the worktree for a resume.
    if orbit_types::workflow::is_provider_capacity_exhausted(None, Some(error_message)) {
        return skip(
            "the provider reported the selected model at capacity; the candidate was not judged",
        );
    }
    let Some(task_id) = single_task_id(&ctx.input) else {
        return skip(
            "final recovery decides for exactly one task; this run carries none or several",
        );
    };
    let Some(worktree) = run_worktree(job, ctx, step_index) else {
        return skip("the run has no assigned worktree for final recovery to inspect");
    };

    if let FinalRecoveryBudget::Replay(decision) =
        std::mem::replace(budget, FinalRecoveryBudget::Spent)
    {
        let application = FinalRecoveryApplication {
            task_id,
            failed_step_id: step.id.clone(),
            decision,
            resume_step_index: None,
            workspace_path: worktree.workspace_path.into(),
            completion_done: ctx.input.get("completion").and_then(Value::as_str) == Some("done"),
        };
        return conclude(job, ctx, step, activity, &application, None);
    }
    let request = FinalRecoveryAdmissionRequest {
        task_id: task_id.clone(),
        failed_step_id: step.id.clone(),
        base_ref: worktree.base_ref.clone(),
    };
    match ctx.host.admit_final_recovery(&ctx.run_id, &request) {
        Ok(FinalRecoveryAdmission::Admitted) => {}
        Ok(FinalRecoveryAdmission::Skipped { reason }) => return skip(&reason),
        Err(error) => return skip(&format!("admission failed: {error}")),
    }

    let dispatch = final_recovery_input(job, ctx, step_index, &task_id, &worktree, error_message)
        .and_then(|input| dispatch_final_recovery(ctx, step, activity, &input));
    let decision = match dispatch {
        Ok(output) => FinalRecoveryDecision::from_output(Some(&decision_payload(&output))),
        Err(message) => FinalRecoveryDecision::Escalate {
            diagnosis: format!(
                "final recovery activity `{}` did not return a decision: {message}",
                activity.name
            ),
            human_action: "Inspect the failed run and its final-recovery invocation, then move \
                           the task by hand."
                .to_string(),
        },
    };
    let (decision, resume_index) = admit_resume_step(job, step_index, decision);

    let application = FinalRecoveryApplication {
        task_id,
        failed_step_id: step.id.clone(),
        decision: decision.clone(),
        resume_step_index: resume_index.map(|index| index as u32),
        workspace_path: worktree.workspace_path.into(),
        completion_done: ctx.input.get("completion").and_then(Value::as_str) == Some("done"),
    };
    conclude(job, ctx, step, activity, &application, resume_index)
}

/// Hand `application` to the host and turn its answer into the verdict.
fn conclude(
    job: &JobV2,
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
    activity: &ResolvedRecoveryActivity,
    application: &FinalRecoveryApplication,
    resume_index: Option<usize>,
) -> FinalRecoveryVerdict {
    let applied = ctx.host.apply_final_recovery(&ctx.run_id, application);
    let kind = Some(application.decision.kind());
    match (applied, resume_index) {
        (Ok(FinalRecoveryApplied::Resume), Some(index)) => {
            let detail = format!("rerunning from step `{}`", job.steps[index].id);
            emit_final_recovery_event(ctx, step, activity, "resume", kind, Some(&detail));
            FinalRecoveryVerdict::Resume(index)
        }
        (Ok(FinalRecoveryApplied::Settled { outcome }), _) => {
            emit_final_recovery_event(ctx, step, activity, "settled", kind, Some(&outcome));
            FinalRecoveryVerdict::Settled
        }
        (Ok(FinalRecoveryApplied::Escalated { outcome }), _) => {
            emit_final_recovery_event(ctx, step, activity, "escalated", kind, Some(&outcome));
            FinalRecoveryVerdict::Escalated
        }
        (Ok(FinalRecoveryApplied::Resume), None) => {
            let detail = "the host answered resume for a decision that names no step";
            emit_final_recovery_event(ctx, step, activity, "escalated", kind, Some(detail));
            FinalRecoveryVerdict::Escalated
        }
        (Err(error), _) => {
            let detail = format!("applying the decision failed: {error}");
            emit_final_recovery_event(ctx, step, activity, "escalated", kind, Some(&detail));
            FinalRecoveryVerdict::Escalated
        }
    }
}

/// The one task this run carries, from `task_id` or a one-element `task_ids`.
fn single_task_id(input: &Value) -> Option<String> {
    let non_empty = |value: &Value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    };
    match input.get("task_ids").and_then(Value::as_array) {
        Some(ids) if ids.len() == 1 => non_empty(&ids[0]),
        Some(ids) if !ids.is_empty() => None,
        _ => input.get("task_id").and_then(non_empty),
    }
}

/// The worktree the earliest completed step before the failure reported:
/// the first output carrying a non-empty `workspace_path`.
fn run_worktree(job: &JobV2, ctx: &ExecCtx<'_>, step_index: usize) -> Option<RunWorktree> {
    let pipeline = ctx.pipeline_snapshot();
    let text = |output: &Value, key: &str| {
        output
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    job.steps[..step_index].iter().find_map(|step| {
        let output = pipeline.get(&step.id)?.get("output")?;
        Some(RunWorktree {
            workspace_path: text(output, "workspace_path")?,
            base_ref: text(output, "base_ref"),
            base_sha: text(output, "base_sha"),
        })
    })
}

fn final_recovery_input(
    job: &JobV2,
    ctx: &ExecCtx<'_>,
    step_index: usize,
    task_id: &str,
    worktree: &RunWorktree,
    error_message: &str,
) -> Result<Value, String> {
    let step = &job.steps[step_index];
    let log_tail = ctx
        .host
        .final_recovery_log_tail(&ctx.run_id)
        .map_err(|error| format!("read final-recovery log tail: {error}"))?
        .unwrap_or_default();
    let log_tail = &log_tail[orbit_common::text::ceil_char_boundary(
        &log_tail,
        log_tail
            .len()
            .saturating_sub(MAX_RECOVERY_ERROR_MESSAGE_BYTES),
    )..];
    let mut input = serde_json::json!({
        "task_id": task_id,
        "run_id": ctx.run_id,
        "workspace_path": worktree.workspace_path,
        "repo_root": worktree.workspace_path,
        "failed_step_id": step.id,
        "activity_name": step_activity_name(step),
        "error_message": bounded_recovery_text(
            "error_message",
            &ctx.run_id,
            error_message,
            MAX_RECOVERY_ERROR_MESSAGE_BYTES,
        ),
        "step_recovery_attempts": step_recovery_attempts(ctx)?,
        "log_tail": bounded_recovery_text(
            "log_tail",
            &ctx.run_id,
            log_tail,
            MAX_RECOVERY_ERROR_MESSAGE_BYTES,
        ),
        "step_outputs": bounded_recovery_input(&ctx.run_id, ctx.pipeline_value()),
        "step_ids": job.steps.iter().map(|step| step.id.clone()).collect::<Vec<_>>(),
        "crew_config_key": FINAL_RECOVERY_CREWS_KEY,
    });
    if let JobV2StepBody::Target(target) = &step.body
        && let Ok(rendered) = render_input(
            target.default_input.as_ref(),
            &ctx.input,
            &ctx.template_ctx(),
            target.input_schema_json.as_ref(),
        )
    {
        input["failed_step_input"] = bounded_recovery_input(&ctx.run_id, rendered);
    }
    for (key, value) in [
        ("base_ref", &worktree.base_ref),
        ("base_sha", &worktree.base_sha),
    ] {
        if let Some(value) = value {
            input[key] = Value::String(value.clone());
        }
    }
    Ok(input)
}

/// Keep each recovery and post-recovery record, oldest first, with bounded
/// diagnostics, dispatch output and the read-back recovery decision. Filter
/// the run even when a host's audit sink returns a broader snapshot.
/// Historical records have no output or decision.
fn step_recovery_attempts(ctx: &ExecCtx<'_>) -> Result<Vec<Value>, String> {
    let mut events = ctx
        .audit
        .events_snapshot()
        .map_err(|error| format!("read final-recovery audit evidence: {error}"))?;
    events.retain(|event| event.envelope.run_id == ctx.run_id);
    events.sort_by(|left, right| {
        left.envelope
            .ts
            .cmp(&right.envelope.ts)
            .then_with(|| left.envelope.event_id.cmp(&right.envelope.event_id))
    });
    let attempts = events
        .into_iter()
        .filter_map(|event| {
            let decision = match &event.kind {
                V2AuditEventKind::StepRecoveryAttempted { decision, .. } => decision.clone(),
                _ => None,
            };
            let (step_id, activity, phase, outcome, failure_phase, error_message, output) =
                match event.kind {
                    V2AuditEventKind::StepRecoveryAttempted {
                        step_id,
                        recovery_activity,
                        recovery_succeeded,
                        failure_phase,
                        error_message,
                        output,
                        ..
                    } => (
                        step_id,
                        recovery_activity,
                        "recovery",
                        if recovery_succeeded {
                            "success"
                        } else {
                            "failed"
                        }
                        .to_string(),
                        failure_phase,
                        error_message,
                        output,
                    ),
                    V2AuditEventKind::StepPostRecoveryAttempt {
                        step_id,
                        recovery_activity,
                        outcome,
                        error_message,
                        output,
                    } => (
                        step_id,
                        recovery_activity,
                        "post_recovery",
                        outcome,
                        None,
                        error_message,
                        output,
                    ),
                    _ => return None,
                };
            Some(bounded_recovery_input(
                &ctx.run_id,
                serde_json::json!({
                    "event_id": event.envelope.event_id,
                    "attempted_at": event.envelope.ts,
                    "failed_step_id": step_id,
                    "activity": activity,
                    "phase": phase,
                    "outcome": outcome,
                    "failure_phase": failure_phase,
                    "error_message": error_message,
                    "output": output,
                    "decision": decision,
                }),
            ))
        })
        .collect::<Vec<_>>();
    // Preserve the array contract and every record. The ordinary leaf bound
    // fits all evidence; a run with too much remaining metadata escalates
    // instead of replacing attempts with a preview or dispatching an
    // unbounded provider envelope.
    match bounded_recovery_input(&ctx.run_id, Value::Array(attempts)) {
        Value::Array(attempts) => Ok(attempts),
        _ => Err("step-recovery evidence exceeds the bounded recovery input; an operator must inspect the full audit trail".to_string()),
    }
}

/// Dispatch the activity under its configured crew, returning its output.
fn dispatch_final_recovery(
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
    activity: &ResolvedRecoveryActivity,
    input: &Value,
) -> Result<Value, String> {
    let spec = crew_overridden_recovery_spec(activity, ctx, input)
        .map_err(|error| format!("crew: {error}"))?;
    let dispatch = dispatch_v2_activity_without_run_id_injection(V2DispatchInput {
        activity_name: &activity.name,
        spec: spec.as_ref().unwrap_or(&activity.spec),
        fs_profile: step_fs_profile(step),
        input: input.clone(),
        audit: ctx.audit.clone(),
        run_id: &ctx.run_id,
        host: Some(ctx.host),
    })
    .map_err(|error| error.to_string())?;
    persist_dispatch_invocation(ctx, &activity.name, input, &dispatch);
    if dispatch.success {
        Ok(dispatch.output)
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
fn decision_payload(output: &Value) -> Value {
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

/// Keep a `resume` only when it names the failed step or an earlier step of
/// its phase; otherwise it becomes `escalate`.
///
/// Phases are top-level steps — the executor's checkpoint and resume unit —
/// so a step nested in a block resumes by naming its block. The job's first
/// step admits the run's tasks and creates its worktree; it is a phase of its
/// own, never rerun to repair a later failure.
fn admit_resume_step(
    job: &JobV2,
    failed_index: usize,
    decision: FinalRecoveryDecision,
) -> (FinalRecoveryDecision, Option<usize>) {
    let FinalRecoveryDecision::Resume { step_id, .. } = &decision else {
        return (decision, None);
    };
    let step_id = step_id.trim();
    let target = job.steps.iter().position(|step| step.id == step_id);
    match target {
        Some(index) if index == failed_index || (1..failed_index).contains(&index) => {
            (decision, Some(index))
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
                },
                None,
            )
        }
    }
}

fn emit_final_recovery_event(
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
    activity: &ResolvedRecoveryActivity,
    outcome: &str,
    decision: Option<&str>,
    detail: Option<&str>,
) {
    emit_job_event_lossy(
        &ctx.audit,
        ctx.task_id(),
        V2AuditEventKind::FinalRecoveryAttempted {
            step_id: step.id.clone(),
            final_recovery_activity: activity.name.clone(),
            outcome: outcome.to_string(),
            decision: decision.map(str::to_string),
            detail: detail.map(redacted_recovery_diagnostic),
        },
    );
}
