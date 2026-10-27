//! Failure-chain and cancellation fields for one run-detail response.
//!
//! These ride on the dashboard's run object only. The shared run projection,
//! the run list, and the CLI keep their existing shapes.

use orbit_core::application::job::{
    RunFailureCause, RunFailureContextProfile, RunFailureContextWarning, RunFailureEvidence,
};
use orbit_core::{JobRun, JobRunState, OrbitRuntime, V2AuditEventFilter};
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};

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
    full["failure_root"] =
        failure_root(runtime, run, state).map_or(Value::Null, |cause| cause.to_failure_root_json());
    full["cancellation"] = if run.state == JobRunState::Cancelled {
        cancellation_value(runtime, run, state)
    } else {
        Value::Null
    };
}

/// The deepest failed descendant, through the shared failure query. Every
/// read this profile makes is supplementary, so it is logged, never raised.
fn failure_root(
    runtime: &OrbitRuntime,
    run: &JobRun,
    state: Option<&PipelineState>,
) -> Option<RunFailureCause> {
    runtime
        .run_failure_context(
            run,
            state,
            RunFailureContextProfile::DeepestObserved,
            &mut log_failure_warning,
        )
        .map_err(|error| {
            tracing::warn!(run_id = %run.run_id, %error, "failure root unavailable");
        })
        .ok()?
        .causes
        .into_iter()
        .next()
}

/// The diagnostic each skipped supplementary read has always logged.
fn log_failure_warning(warning: &RunFailureContextWarning) {
    let error = &warning.error;
    match warning.evidence {
        RunFailureEvidence::ChildRun => tracing::warn!(
            child_run_id = %warning.run_id,
            %error,
            "failure root skipped a child run"
        ),
        RunFailureEvidence::Checkpoint => tracing::warn!(
            run_id = %warning.run_id,
            %error,
            "failure root continued without a child's pipeline state"
        ),
        RunFailureEvidence::AuditSteps => tracing::warn!(
            run_id = %warning.run_id,
            %error,
            "failure root omitted audit steps"
        ),
    }
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

fn cancellation_value(
    runtime: &OrbitRuntime,
    run: &JobRun,
    state: Option<&PipelineState>,
) -> Value {
    if let Some(cancel) = state.and_then(|state| state.pull_drain.drain_cancel.as_ref()) {
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

fn owned(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn reason_text(reason: Option<&str>) -> String {
    owned(reason).unwrap_or_else(|| NO_REASON_RECORDED.to_string())
}

/// One line naming why `run` stopped, in the same shape the run page prints.
///
/// A failed, timed-out, interrupted, or cancelled run uses the deepest cause
/// in its dispatch chain. Any other state, including a hold, uses this run's
/// own recorded step and message. Returns none when neither source recorded
/// a step or a message, so a caller can say the cause is unavailable.
pub(crate) fn failure_root_line(runtime: &OrbitRuntime, run: &JobRun) -> Option<String> {
    let state = read_state(runtime, &run.run_id);
    let chain_line = failure_root(runtime, run, state.as_ref()).and_then(|cause| {
        format_cause_line(
            &cause.run_id,
            &cause.state.to_string(),
            cause.step.as_deref(),
            cause.message.as_deref(),
        )
    });
    if chain_line.is_some() {
        return chain_line;
    }
    let cause = runtime
        .run_failure_cause(
            run,
            RunFailureContextProfile::DeepestObserved,
            &mut log_failure_warning,
        )
        .ok()?;
    format_cause_line(
        &run.run_id,
        &run.state.to_string(),
        cause.step.as_deref(),
        cause.message.as_deref(),
    )
}

fn format_cause_line(
    run_id: &str,
    state: &str,
    step: Option<&str>,
    message: Option<&str>,
) -> Option<String> {
    let step = step.map(str::trim).filter(|text| !text.is_empty());
    let message = message.map(str::trim).filter(|text| !text.is_empty());
    if step.is_none() && message.is_none() {
        return None;
    }
    let mut line = format!("{run_id} {state}");
    if let Some(step) = step {
        line.push_str(" at ");
        line.push_str(step);
    }
    if let Some(message) = message {
        line.push_str(" - ");
        line.push_str(message);
    }
    Some(line)
}
