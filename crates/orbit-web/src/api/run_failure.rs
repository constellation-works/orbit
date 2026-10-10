//! Failure-chain and cancellation fields for one run-detail response.
//!
//! These ride on the dashboard's run object only. The shared run projection,
//! the run list, and the CLI keep their existing shapes.

use std::collections::HashSet;

use orbit_core::runtime::audit::run::RunAuditStep;
use orbit_core::{JobRun, JobRunState, OrbitRuntime, V2AuditEventFilter};
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};

/// How far the dashboard follows child dispatches. Matches the cancel cascade.
const MAX_FAILURE_ROOT_DEPTH: usize = 8;
/// Exact phrase when a cancelled run has no recorded reason.
const NO_REASON_RECORDED: &str = "no reason recorded";
const CANCELLATION_NOTE_PREFIX: &str = "run cancelled by ";

/// Attach `failure_root` and `cancellation` to the detail run object.
///
/// `failure_root` is the deepest failed, timed-out, cancelled, or interrupted
/// descendant, or null when this run has none. A successful or skipped child
/// is not followed, so a drain of successful leaves is not walked. Ties at the
/// same depth keep the earlier child dispatch. `cancellation` is set only for
/// a cancelled run. Its `reason` is the recorded text, or [`NO_REASON_RECORDED`]
/// when the record has none. A failed supplementary read is logged and left
/// out; it does not fail the detail page.
pub(super) fn attach_run_failure_context(
    runtime: &OrbitRuntime,
    run: &JobRun,
    state: Option<&PipelineState>,
    full: &mut Value,
) {
    full["failure_root"] = failure_root_value(runtime, run, state);
    full["cancellation"] = if run.state == JobRunState::Cancelled {
        cancellation_value(runtime, run, state)
    } else {
        Value::Null
    };
}

fn is_chain_failure(state: JobRunState) -> bool {
    matches!(
        state,
        JobRunState::Failed
            | JobRunState::Timeout
            | JobRunState::Interrupted
            | JobRunState::Cancelled
    )
}

struct ChainFailure {
    run: JobRun,
    depth: usize,
}

fn failure_root_value(
    runtime: &OrbitRuntime,
    run: &JobRun,
    state: Option<&PipelineState>,
) -> Value {
    if !is_chain_failure(run.state) {
        return Value::Null;
    }
    let mut visited = HashSet::new();
    let Some(cause) = walk(runtime, run, state, &mut visited, 0) else {
        return Value::Null;
    };
    let (step, message) = cause_summary(runtime, &cause.run);
    json!({
        "run_id": cause.run.run_id,
        "state": cause.run.state.to_string(),
        "step": step,
        "message": message,
    })
}

/// Depth 0 is the run being viewed. It is never its own root cause.
fn walk(
    runtime: &OrbitRuntime,
    run: &JobRun,
    state: Option<&PipelineState>,
    visited: &mut HashSet<String>,
    depth: usize,
) -> Option<ChainFailure> {
    if depth > MAX_FAILURE_ROOT_DEPTH {
        return None;
    }
    if !visited.insert(run.run_id.clone()) {
        return None;
    }
    // A successful or skipped descendant is not a cause, and following it
    // would walk a drain's finished leaves to find an unrelated failure.
    if depth > 0 && matches!(run.state, JobRunState::Success | JobRunState::Skipped) {
        return None;
    }
    let mut best = None;
    if let Some(state) = state {
        for dispatch in &state.child_dispatches {
            let Some(child) = show_child(runtime, &dispatch.child_run_id) else {
                continue;
            };
            let child_state = read_state(runtime, &child.run_id);
            if let Some(found) = walk(runtime, &child, child_state.as_ref(), visited, depth + 1) {
                best = Some(prefer(best, found));
            }
        }
    }
    if depth > 0 && best.is_none() && is_chain_failure(run.state) {
        best = Some(ChainFailure {
            run: run.clone(),
            depth,
        });
    }
    best
}

fn prefer(current: Option<ChainFailure>, next: ChainFailure) -> ChainFailure {
    match current {
        Some(current) if next.depth > current.depth => next,
        Some(current) => current,
        None => next,
    }
}

fn show_child(runtime: &OrbitRuntime, run_id: &str) -> Option<JobRun> {
    // Observed, not reconciled: naming a cause must not finalize a child.
    runtime
        .show_job_run_observed(run_id)
        .map_err(|error| {
            tracing::warn!(
                child_run_id = %run_id,
                %error,
                "failure root skipped a child run"
            );
        })
        .ok()
}

fn read_state(runtime: &OrbitRuntime, run_id: &str) -> Option<PipelineState> {
    runtime.read_run_state(run_id).unwrap_or_else(|error| {
        tracing::warn!(
            run_id,
            %error,
            "failure root continued without a child's pipeline state"
        );
        None
    })
}

fn cause_summary(runtime: &OrbitRuntime, run: &JobRun) -> (Option<String>, Option<String>) {
    let audit = match runtime.collect_run_audit_steps(&run.run_id) {
        Ok(steps) => steps,
        Err(error) => {
            tracing::warn!(
                run_id = %run.run_id,
                %error,
                "failure root omitted audit steps"
            );
            Vec::new()
        }
    };
    if let Some(summary) = audit_summary(&audit) {
        return summary;
    }
    stored_summary(run)
}

fn audit_summary(steps: &[RunAuditStep]) -> Option<(Option<String>, Option<String>)> {
    let chosen = steps
        .iter()
        .rev()
        .find(|step| nonempty(step.error_message.as_deref()))
        .or_else(|| {
            steps.iter().rev().find(|step| {
                matches!(
                    step.state.as_deref(),
                    Some("error" | "failed" | "timeout" | "interrupted" | "cancelled")
                ) && nonempty(Some(step.step_id.as_str()))
            })
        })?;
    let summary = (
        owned(Some(chosen.step_id.as_str())),
        owned(chosen.error_message.as_deref()),
    );
    (summary.0.is_some() || summary.1.is_some()).then_some(summary)
}

fn stored_summary(run: &JobRun) -> (Option<String>, Option<String>) {
    let Some(chosen) = run
        .steps
        .iter()
        .rev()
        .find(|step| step.state != JobRunState::Skipped && nonempty(step.error_message.as_deref()))
        .or_else(|| {
            run.steps
                .iter()
                .rev()
                .find(|step| is_chain_failure(step.state))
        })
        .or_else(|| {
            run.steps
                .iter()
                .rev()
                .find(|step| step.state != JobRunState::Skipped)
        })
    else {
        return (None, None);
    };
    (
        owned(Some(chosen.target_id.as_str())),
        owned(chosen.error_message.as_deref()),
    )
}

fn cancellation_value(
    runtime: &OrbitRuntime,
    run: &JobRun,
    state: Option<&PipelineState>,
) -> Value {
    if let Some(cancel) = state.and_then(|state| state.drain_cancel.as_ref()) {
        return json!({
            "actor": owned(Some(cancel.actor.as_str())),
            "source": owned(Some(cancel.source.as_str())),
            "reason": reason_text(cancel.reason.as_deref()),
            "at": cancel.requested_at,
        });
    }
    if let Some(value) = cancellation_from_audit(runtime, run) {
        return value;
    }
    if let Some(policy) = state.and_then(|state| state.task_cancellation_policy.as_ref())
        && let Some((actor, reason)) = parse_cancellation_note(&policy.note)
    {
        return json!({
            "actor": actor,
            "source": Value::Null,
            "reason": reason_text(reason.as_deref()),
            "at": run.finished_at,
        });
    }
    json!({
        "actor": Value::Null,
        "source": Value::Null,
        "reason": NO_REASON_RECORDED,
        "at": run.finished_at,
    })
}

fn cancellation_from_audit(runtime: &OrbitRuntime, run: &JobRun) -> Option<Value> {
    let rows = runtime
        .list_v2_audit_events(V2AuditEventFilter {
            workspace_id: String::new(),
            run_id: Some(run.run_id.clone()),
            event_type: Some("run.cancelled".to_string()),
            source: Some("v2_envelope".to_string()),
            limit: Some(1),
            oldest_first: false,
            ..V2AuditEventFilter::default()
        })
        .map_err(|error| {
            tracing::warn!(
                run_id = %run.run_id,
                %error,
                "run detail omitted the cancellation audit"
            );
        })
        .ok()?;
    let row = rows.into_iter().next()?;
    let payload = serde_json::from_str::<Value>(&row.payload_json).unwrap_or_else(|error| {
        tracing::warn!(
            run_id = %run.run_id,
            %error,
            "run detail could not read the cancellation audit"
        );
        Value::Null
    });
    let actor = json_text(&payload, "actor").or_else(|| owned(Some(row.agent_identity.as_str())));
    Some(json!({
        "actor": actor,
        "source": json_text(&payload, "source"),
        "reason": reason_text(json_text(&payload, "reason").as_deref()),
        "at": row.ts,
    }))
}

/// `cancellation_note` writes `run cancelled by <actor>` or
/// `run cancelled by <actor>: <reason>`. The reason may itself contain colons.
fn parse_cancellation_note(note: &str) -> Option<(String, Option<String>)> {
    let rest = note.trim().strip_prefix(CANCELLATION_NOTE_PREFIX)?;
    let (actor, reason) = match rest.split_once(": ") {
        Some((actor, reason)) => (actor, owned(Some(reason))),
        None => (rest, None),
    };
    Some((owned(Some(actor))?, reason))
}

fn json_text(value: &Value, key: &str) -> Option<String> {
    owned(value.get(key).and_then(Value::as_str))
}

fn nonempty(value: Option<&str>) -> bool {
    value.is_some_and(|text| !text.trim().is_empty())
}

fn owned(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn reason_text(reason: Option<&str>) -> String {
    owned(reason).unwrap_or_else(|| NO_REASON_RECORDED.to_string())
}
