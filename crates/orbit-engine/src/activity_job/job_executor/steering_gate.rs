//! Mid-run steering's promotion gate
//! (`docs/design/mid-run-steering/2_design.md` §3).
//!
//! A job that declares `steering_gate` asks the host, before its `before`
//! top-level step, whether an operator comment past the run's steering
//! watermark is still undelivered. The host answers from the durable task and
//! run records alone; nothing an agent returned is consulted. With steering
//! off the host reads nothing and the step runs exactly as it would without
//! the gate.
//!
//! Undelivered steering does not reach `before`. The host admits one steering
//! round, recording it on the run before anything is dispatched, and the run
//! goes back to the gate's `round` step, which runs even when its `when:` is
//! false. Its agent step's next prompt is a safe point, so it carries the
//! steering block, and building that prompt is what moves the watermark. A
//! round that fails takes the run's ordinary failure path with its own
//! failure. Back at the gate, the comment log is read again.
//!
//! A run that carries a candidate past its implementer built no prompt for
//! it. When the gate names its `candidate` step, the run that step says the
//! candidate came from is handed to the host with each reading and round, so
//! the host judges from that run's baseline rather than this run's start.
//!
//! Once the host refuses another round — the run spent
//! `workflow.mid_run_steering.max_rounds`, or the last round was admitted for
//! the very comments still outstanding and delivered none of them — the task
//! is blocked under [`STEERING_UNCONSUMED_EVENT`] naming those comments, and
//! the run ends without reaching `before`. A gate that cannot read the
//! records refuses the same way, without blocking the task itself. A task that
//! went terminal stops the run before `before` too, reporting the steering it
//! never received; terminal handling wins. None of these stops runs final
//! recovery or the job's failure activity: the candidate stays uncommitted in
//! its worktree, as the gate found it.

use chrono::SecondsFormat;
use orbit_common::OrbitError;
use orbit_types::task::{TaskComment, TaskStatus};
use orbit_types::workflow::activity_job::SteeringGateSpec;
use orbit_types::workflow::{
    CommentLogMark, STEERING_UNCONSUMED_EVENT, STEERING_UNCONSUMED_MARKER,
};
use serde_json::json;

use super::*;
use crate::context::{SteeringGateReading, SteeringRoundAdmission, TaskAutomationUpdate};

/// The run-input key a steering round's targets receive: the round, the
/// run's budget and the comments the gate found undelivered.
pub(super) const STEERING_ROUND_KEY: &str = "steering_round";

/// What the executor does at a top-level step after the gate.
pub(super) enum SteeringGateOutcome {
    /// Run the step.
    Proceed,
    /// Run again from top-level step `index`, handing it `context`.
    Round { index: usize, context: Value },
    /// End the run unsuccessfully with this message before the step runs.
    Stop(String),
}

/// Apply the job's steering gate, if it guards `step`.
pub(super) fn run_steering_gate(
    job: &JobV2,
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
) -> SteeringGateOutcome {
    let Some(gate) = job
        .steering_gate
        .as_ref()
        .filter(|gate| gate.before == step.id)
    else {
        return SteeringGateOutcome::Proceed;
    };
    let candidate = carried_candidate_source(gate, ctx);
    let candidate_source = candidate.as_ref().ok().and_then(Option::as_deref);
    let mut outstanding = Vec::new();
    for task_id in gate_task_ids(&ctx.input) {
        let reading = ctx
            .host
            .steering_gate(&ctx.run_id, &task_id, candidate_source)
            .and_then(|reading| match (&candidate, reading) {
                // A carried candidate without its source has no baseline, but
                // steering off still reads nothing.
                (Err(error), reading) if reading != SteeringGateReading::Disabled => {
                    Err(OrbitError::Execution(error.clone()))
                }
                (_, reading) => Ok(reading),
            });
        match reading {
            Ok(SteeringGateReading::Disabled) => return SteeringGateOutcome::Proceed,
            Ok(SteeringGateReading::Clear) => {}
            Ok(SteeringGateReading::Terminal {
                outstanding: comments,
                ..
            }) if comments.is_empty() => {}
            Ok(SteeringGateReading::Terminal {
                status,
                outstanding: comments,
            }) => {
                return stop(
                    ctx,
                    step,
                    json!({
                        "decision": "task_terminal",
                        "task_id": task_id,
                        "task_status": status.cli_name(),
                        "outstanding": comments,
                    }),
                    format!(
                        "step `{}` did not run: task {task_id} is {}, so the run stops as \
                         terminal handling requires; operator steering it never received: {}",
                        step.id,
                        status.cli_name(),
                        describe(&comments)
                    ),
                );
            }
            Ok(SteeringGateReading::Outstanding(comments)) => outstanding.push((task_id, comments)),
            Err(error) => {
                return stop(
                    ctx,
                    step,
                    json!({
                        "decision": "unreadable",
                        "task_id": task_id,
                        "error": error.to_string(),
                    }),
                    format!(
                        "[steering_gate_unreadable] step `{}` did not run: the promotion gate \
                         could not read task {task_id} for run {}: {error}. Nothing was \
                         committed; the candidate stays in its worktree",
                        step.id, ctx.run_id
                    ),
                );
            }
        }
    }
    if outstanding.is_empty() {
        return SteeringGateOutcome::Proceed;
    }
    let Some(round_index) = job.steps.iter().position(|step| step.id == gate.round) else {
        return stop(
            ctx,
            step,
            json!({ "decision": "unroutable", "round_step": gate.round }),
            format!(
                "step `{}` did not run: operator steering is undelivered and the steering \
                 round step `{}` is not a top-level step of this job",
                step.id, gate.round
            ),
        );
    };

    let mut admitted = None;
    for (task_id, comments) in &outstanding {
        match ctx
            .host
            .admit_steering_round(&ctx.run_id, task_id, comments, candidate_source)
        {
            Ok(SteeringRoundAdmission::Admitted { round, max_rounds }) => {
                admitted = Some((round, max_rounds));
            }
            Ok(
                refusal @ (SteeringRoundAdmission::Exhausted { rounds, max_rounds }
                | SteeringRoundAdmission::Unobservable { rounds, max_rounds }),
            ) => {
                let unobservable = matches!(refusal, SteeringRoundAdmission::Unobservable { .. });
                return hold_unconsumed(
                    ctx,
                    step,
                    task_id,
                    comments,
                    rounds,
                    max_rounds,
                    unobservable,
                );
            }
            Err(error) => {
                return stop(
                    ctx,
                    step,
                    json!({
                        "decision": "unadmitted",
                        "task_id": task_id,
                        "outstanding": comments,
                        "error": error.to_string(),
                    }),
                    format!(
                        "[steering_gate_unreadable] step `{}` did not run: operator steering for \
                         task {task_id} is undelivered ({}) and the steering round could not be \
                         recorded: {error}. Nothing was committed; the candidate stays in its \
                         worktree",
                        step.id,
                        describe(comments)
                    ),
                );
            }
        }
    }
    let (round, max_rounds) = admitted.unwrap_or((0, 0));
    tracing::info!(
        target: "orbit.engine.job_executor",
        run_id = %ctx.run_id,
        gate_step = %step.id,
        round_step = %gate.round,
        round,
        max_rounds,
        "steering round admitted at the promotion gate",
    );
    SteeringGateOutcome::Round {
        index: round_index,
        context: json!({
            "round": round,
            "max_rounds": max_rounds,
            "gate_step": step.id,
            "outstanding": outstanding
                .iter()
                .map(|(task_id, comments)| json!({ "task_id": task_id, "comments": comments }))
                .collect::<Vec<_>>(),
        }),
    }
}

/// The run whose candidate this run carries past its implementer, as the
/// gate's `candidate` step recorded it. A step that reports `implement:
/// false` without naming that run leaves the gate no baseline, which is an
/// error rather than a reading from this run's start.
fn carried_candidate_source(
    gate: &SteeringGateSpec,
    ctx: &ExecCtx<'_>,
) -> Result<Option<String>, String> {
    let Some(step) = gate.candidate.as_deref() else {
        return Ok(None);
    };
    let Some(output) = ctx.pipeline_snapshot().get(step).map(unwrap_step_output) else {
        return Ok(None);
    };
    if output.get("implement").and_then(Value::as_bool) != Some(false) {
        return Ok(None);
    }
    output
        .get("source_run_id")
        .and_then(Value::as_str)
        .filter(|source| !source.is_empty())
        .map(|source| Some(source.to_string()))
        .ok_or_else(|| {
            format!(
                "step `{step}` carries a candidate past the implementer without naming the run \
                 it came from, so its steering baseline cannot be resolved"
            )
        })
}

/// Block `task_id` under [`STEERING_UNCONSUMED_EVENT`] and stop the run.
fn hold_unconsumed(
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
    task_id: &str,
    comments: &[CommentLogMark],
    rounds: u8,
    max_rounds: u8,
    unobservable: bool,
) -> SteeringGateOutcome {
    let why = if unobservable {
        format!(
            "its last steering round ({rounds} of {max_rounds}) delivered none of them, so \
             another would observe nothing new"
        )
    } else {
        format!("the run spent all {max_rounds} steering rounds")
    };
    let note = format!(
        "{STEERING_UNCONSUMED_MARKER} run={}, gate_step={}, rounds={rounds}/{max_rounds}: \
         operator steering stayed undelivered and {why}; outstanding: {}. Nothing was \
         committed; the candidate stays in its worktree",
        ctx.run_id,
        step.id,
        describe(comments)
    );
    let blocked = ctx.host.apply_task_automation_update(
        task_id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Blocked),
            status_event: Some(STEERING_UNCONSUMED_EVENT.to_string()),
            status_note: Some(note.clone()),
            append_comments: vec![TaskComment {
                at: chrono::Utc::now(),
                by: "system".to_string(),
                message: format!(
                    "{note}.\n\nRead the outstanding comments with `orbit task show {task_id}`, \
                     then requeue the task, answer them differently, or accept the candidate \
                     as it is through the normal review record."
                ),
                operation_id: None,
            }],
            ..TaskAutomationUpdate::default()
        },
    );
    let mut message = note;
    if let Err(error) = &blocked {
        message.push_str(&format!("; blocking task {task_id} failed: {error}"));
    }
    stop(
        ctx,
        step,
        json!({
            "decision": STEERING_UNCONSUMED_EVENT,
            "task_id": task_id,
            "outstanding": comments,
            "rounds": rounds,
            "max_rounds": max_rounds,
            "task_blocked": blocked.is_ok(),
        }),
        message,
    )
}

/// Record the gate's refusal as `step`'s outcome, then stop.
fn stop(
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
    mut outcome: Value,
    message: String,
) -> SteeringGateOutcome {
    if let Some(object) = outcome.as_object_mut() {
        object.insert("gate".to_string(), json!("steering"));
    }
    tracing::warn!(
        target: "orbit.engine.job_executor",
        run_id = %ctx.run_id,
        gate_step = %step.id,
        %message,
        "promotion gate stopped the run before its step",
    );
    record_pipeline(ctx, &step.id, outcome);
    SteeringGateOutcome::Stop(message)
}

/// The run's tasks, as its input names them.
fn gate_task_ids(input: &Value) -> Vec<String> {
    let listed: Vec<String> = input
        .get("task_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    if !listed.is_empty() {
        return listed;
    }
    input
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .into_iter()
        .collect()
}

/// Comment identities as an operator reads them: log position and recorded
/// timestamp.
fn describe(comments: &[CommentLogMark]) -> String {
    comments
        .iter()
        .map(|mark| {
            format!(
                "comment {} at {}",
                mark.position,
                mark.at.to_rfc3339_opts(SecondsFormat::Secs, true)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}
