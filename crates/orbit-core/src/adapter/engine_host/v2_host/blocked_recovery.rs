//! Deterministic steps of `blocked_task_recovery_pipeline`.
//!
//! `prepare_blocked_task_recovery` re-checks the episode and builds the
//! `final_recovery` agent's input; `apply_blocked_task_recovery` hands the
//! agent's decision to the shared applier and removes the run's checkout. The
//! decisions themselves live in `application::task::blocked_recovery`.

use orbit_engine::DispatchError;
use serde_json::{Map, Value, json};

use crate::OrbitRuntime;
use crate::application::task::{
    BlockedRecoveryInput, BlockedRecoveryPreparation, FinalRecoveryOutcome,
};

/// `prepare_blocked_task_recovery`: `proceed: false` with a `reason` when the
/// episode moved on, otherwise the agent's input under `recovery` and the
/// episode echoed under `episode` for the apply step.
pub(super) fn prepare(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    recovery_run_id: Option<&str>,
) -> Result<Value, DispatchError> {
    let run_id = recovery_run_id
        .ok_or_else(|| failed(action, "a recovery step must run inside a pipeline run"))?;
    let episode = BlockedRecoveryInput::from_json(input).map_err(|error| failed(action, error))?;
    let preparation = runtime
        .prepare_blocked_task_recovery(&episode, run_id)
        .map_err(|error| failed(action, error.to_string()))?;
    Ok(match preparation {
        BlockedRecoveryPreparation::Skip { reason } => json!({
            "proceed": false,
            "reason": reason,
            "episode": episode.to_json(),
        }),
        BlockedRecoveryPreparation::Ready(prepared) => {
            let checkout = prepared.checkout.to_string_lossy().to_string();
            json!({
                "proceed": true,
                "episode": episode.to_json(),
                "recovery": {
                    "task_id": episode.task_id,
                    "run_id": prepared.failed_run_id,
                    "workspace_path": checkout,
                    "repo_root": checkout,
                    "failed_step_id": prepared.failed_step_id,
                    "activity_name": prepared.job_id.unwrap_or_default(),
                    "error_message": prepared.error_message,
                    "base_ref": prepared.base_ref,
                    "base_sha": prepared.base_sha,
                },
            })
        }
    })
}

/// `apply_blocked_task_recovery`: apply the agent's decision and report what
/// the applier did.
pub(super) fn apply(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    recovery_run_id: Option<&str>,
) -> Result<Value, DispatchError> {
    let run_id = recovery_run_id
        .ok_or_else(|| failed(action, "a recovery step must run inside a pipeline run"))?;
    let prepared = input
        .get("prepared")
        .ok_or_else(|| failed(action, "`prepared` is required"))?;
    let episode = prepared
        .get("episode")
        .ok_or_else(|| failed(action, "`prepared.episode` is required"))
        .and_then(|value| {
            BlockedRecoveryInput::from_json(value).map_err(|error| failed(action, error))
        })?;
    let base_ref = prepared
        .pointer("/recovery/base_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| failed(action, "`prepared.recovery.base_ref` is required"))?;
    let decision = input.get("result").and_then(decision_result);
    let outcome =
        runtime.apply_blocked_task_recovery(&episode, run_id, base_ref, decision.as_ref());
    // The checkout is the run's own scratch; remove it whatever the outcome.
    let cleanup = runtime.remove_recovery_checkout(run_id);
    let outcome = outcome.map_err(|error| failed(action, error.to_string()))?;
    cleanup.map_err(|error| failed(action, error.to_string()))?;
    Ok(json!({
        "status": "succeeded",
        "task_id": episode.task_id,
        "decision": decision
            .as_ref()
            .and_then(|value| value.get("decision"))
            .cloned()
            .unwrap_or(Value::Null),
        "outcome": outcome_name(&outcome),
    }))
}

/// The decision object an agent step returned: exactly the keys of its
/// response envelope's `result`, without the invocation metadata Orbit merges
/// into the step output. `None` when the envelope was missing or invalid.
pub(super) fn decision_result(output: &Value) -> Option<Value> {
    if output
        .get("response_envelope_valid")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return None;
    }
    let fields = output.get("response_result_fields")?.as_array()?;
    let mut decision = Map::new();
    for field in fields {
        let field = field.as_str()?;
        decision.insert(field.to_string(), output.get(field)?.clone());
    }
    Some(Value::Object(decision))
}

fn outcome_name(outcome: &FinalRecoveryOutcome) -> &'static str {
    match outcome {
        FinalRecoveryOutcome::Resume { .. } => "resume",
        FinalRecoveryOutcome::Completed { .. } => "completed",
        FinalRecoveryOutcome::Rejected => "rejected",
        FinalRecoveryOutcome::Archived => "archived",
        FinalRecoveryOutcome::Requeued => "requeued",
        FinalRecoveryOutcome::Escalated { .. } => "escalated",
        FinalRecoveryOutcome::Refused { .. } => "refused",
    }
}

fn failed(action: &str, message: impl Into<String>) -> DispatchError {
    DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message: message.into(),
    }
}
