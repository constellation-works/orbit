//! Pipeline success guard and its terminal-result audit.
//!
//! [ORB-15202] A failed guard records the leaf it echoes on its own run
//! (`root_cause`) and names that leaf instead of nesting the child's text;
//! children that were all cancelled end the parent cancelled, not failed.

use orbit_common::observability::audit_id::audit_execution_id;
use orbit_engine::DispatchError;
use orbit_store::contracts::AuditEventInsertParams;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::RunRootCause;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::job::pipeline::{
    pipeline_wait_status_is_held, pipeline_wait_status_is_success,
};

use super::action_failed;

pub(in super::super) fn pipeline_success_guard(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let allow_non_success = input
        .get("allow_non_success")
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                action_failed(action, "`allow_non_success` must be a boolean".to_string())
            })
        })
        .transpose()?
        .unwrap_or(false);
    if allow_non_success {
        return record_pipeline_results(action, input);
    }

    let context = input
        .get("context")
        .and_then(Value::as_str)
        .unwrap_or("pipeline child run");
    let mut entries = Vec::new();
    if let Some(result) = input.get("result")
        && !result.is_null()
    {
        entries.push(("result".to_string(), result));
    }
    if let Some(results) = input.get("results")
        && !results.is_null()
    {
        let results =
            results
                .as_array()
                .ok_or_else(|| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: "`results` must be an array".to_string(),
                })?;
        entries.extend(
            results
                .iter()
                .enumerate()
                .map(|(idx, entry)| (format!("results[{idx}]"), entry)),
        );
    }
    if entries.is_empty() {
        return Err(DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: "expected `result` or `results` to check".to_string(),
        });
    }

    let held_count = entries
        .iter()
        .filter(|(_, entry)| entry_is_held(entry))
        .count();
    let mut failures = Vec::new();
    let mut root_causes = Vec::new();
    let mut all_cancelled = true;
    for (label, entry) in &entries {
        let cancelled = entry.get("status").and_then(Value::as_str) == Some("cancelled");
        // [ORB-15202] A failed child stands for the leaf it echoes: name that
        // leaf instead of nesting its parents' copies of the leaf's text.
        let root_cause = (!cancelled)
            .then(|| entry_root_cause(runtime, entry))
            .flatten();
        let Some(failure) = pipeline_wait_entry_failure(label, entry, root_cause.as_ref()) else {
            continue;
        };
        all_cancelled &= cancelled;
        failures.push(failure);
        root_causes.extend(root_cause);
    }

    if !failures.is_empty() && all_cancelled {
        // An operator cancelled every child that did not succeed: the parent
        // ends cancelled with them, not failed.
        return Err(DispatchError::ChildRunCancelled {
            message: format!("{context} cancelled: {}", failures.join("; ")),
        });
    }
    if !failures.is_empty() {
        if let (Some(parent), Some(cause)) = (parent_run_id(input), root_causes.first()) {
            runtime.record_run_root_cause(parent, cause);
        }
        return Err(DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("{context} did not succeed: {}", failures.join("; ")),
        });
    }

    Ok(serde_json::json!({
        "succeeded": true,
        "checked_count": entries.len(),
        "held_count": held_count,
    }))
}

/// The run this guard step belongs to. The dispatcher exposes it as
/// `job_run_id` when the input already carried a `run_id` of its own.
fn parent_run_id(input: &Value) -> Option<&str> {
    ["job_run_id", "run_id"]
        .iter()
        .find_map(|field| input.get(*field).and_then(Value::as_str))
        .filter(|run_id| !run_id.trim().is_empty())
}

/// The leaf failure a failed child entry stands for: the root cause its run
/// recorded, else the child run itself. A child whose run is not recorded
/// still stands for itself; an entry without a run id stands for nothing.
fn entry_root_cause(runtime: &OrbitRuntime, entry: &Value) -> Option<RunRootCause> {
    let status = entry.get("status").and_then(Value::as_str)?;
    if pipeline_wait_status_is_success(status) || pipeline_wait_status_is_held(status) {
        return None;
    }
    let run_id = entry
        .get("run_id")
        .and_then(Value::as_str)
        .filter(|run_id| !run_id.trim().is_empty())?;
    let recorded = runtime.run_root_cause(run_id).unwrap_or_else(|error| {
        tracing::warn!(run_id, %error, "could not read a failed child's root cause");
        None
    });
    let mut cause = recorded.unwrap_or_else(|| RunRootCause {
        leaf_run_id: run_id.to_string(),
        task_id: None,
        step: None,
        code: None,
    });
    // A child the wait gave up on has no failure of its own to name.
    if cause.leaf_run_id == run_id && cause.code.is_none() && status != "failed" {
        cause.code = Some(status.to_string());
    }
    Some(cause)
}

/// A held child is awaiting evidence or a forge, not failing: the parent
/// guard passes and the held run's own task state carries the wait [ORB-14748].
fn entry_is_held(entry: &Value) -> bool {
    entry
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(pipeline_wait_status_is_held)
}

/// Validate and retain terminal child results without converting a child
/// failure into a failure of the workspace-level sequencer.
///
/// This is deliberately an opt-in policy on the existing guard action. Gate
/// and wrapper pipelines keep their fail-fast behavior; only a caller
/// that explicitly asks to record terminal non-successes receives counts and
/// the exact entries it supplied. Structural problems remain errors because a
/// missing run id or non-terminal status is not an observed leaf outcome.
fn record_pipeline_results(action: &str, input: &Value) -> Result<Value, DispatchError> {
    let results = input
        .get("results")
        .and_then(|value| (!value.is_null()).then_some(value))
        .ok_or_else(|| action_failed(action, "expected `results` to record".to_string()))?
        .as_array()
        .ok_or_else(|| action_failed(action, "`results` must be an array".to_string()))?;
    if results.is_empty() {
        return Err(action_failed(
            action,
            "expected at least one `results` entry to record".to_string(),
        ));
    }

    let mut succeeded_count = 0usize;
    let mut non_success_count = 0usize;
    let mut held_count = 0usize;
    for (idx, entry) in results.iter().enumerate() {
        let label = format!("results[{idx}]");
        let run_id = entry
            .get("run_id")
            .and_then(Value::as_str)
            .filter(|run_id| !run_id.trim().is_empty())
            .ok_or_else(|| {
                action_failed(action, format!("{label} missing non-empty string run_id"))
            })?;
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| action_failed(action, format!("{label} missing string status")))?;
        if let Some(error) = entry.get("error")
            && !error.is_null()
            && !error.is_string()
        {
            return Err(action_failed(
                action,
                format!("{label} run {run_id} has non-string error"),
            ));
        }
        match status {
            status if pipeline_wait_status_is_success(status) => succeeded_count += 1,
            status if pipeline_wait_status_is_held(status) => held_count += 1,
            "failed" | "cancelled" | "interrupted" | "timeout" => non_success_count += 1,
            other => {
                return Err(action_failed(
                    action,
                    format!("{label} run {run_id} has non-terminal status {other}"),
                ));
            }
        }
    }

    Ok(serde_json::json!({
        "succeeded": non_success_count == 0,
        "checked_count": results.len(),
        "succeeded_count": succeeded_count,
        "non_success_count": non_success_count,
        "held_count": held_count,
        "results": results,
    }))
}

/// Persist each opt-in result-accounting batch independently of the loop's
/// same-id pipeline key, which is overwritten by the next iteration.
/// Parent run state still owns child linkage; this audit row owns the batch's
/// exact result list and aggregate counts for durable history readers.
pub(in super::super) fn record_pipeline_results_audit(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    output: &Value,
) -> Result<(), DispatchError> {
    if input.get("allow_non_success").and_then(Value::as_bool) != Some(true) {
        return Ok(());
    }

    let parent_run_id = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let payload = serde_json::json!({
        "parent_run_id": parent_run_id,
        "parent_step_id": input.get("step_id"),
        "context": input.get("context"),
        "checked_count": output.get("checked_count"),
        "succeeded_count": output.get("succeeded_count"),
        "non_success_count": output.get("non_success_count"),
        "held_count": output.get("held_count"),
        "results": output.get("results"),
    });
    let arguments_json = serde_json::to_string(&payload).map_err(|error| {
        action_failed(
            action,
            format!("serialize pipeline.child_results payload: {error}"),
        )
    })?;

    runtime
        .record_audit_event(&AuditEventInsertParams {
            execution_id: audit_execution_id("audit-pipeline-child-results"),
            command: "pipeline.child_results".to_string(),
            subcommand: None,
            tool_name: None,
            target_type: Some("job_run".to_string()),
            target_id: parent_run_id.clone(),
            role: "admin".to_string(),
            status: AuditEventStatus::Success,
            exit_code: 0,
            duration_ms: 0,
            working_directory: runtime.paths().repo_root.to_string_lossy().into_owned(),
            arguments_json: Some(arguments_json),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: None,
            host: std::env::var("HOSTNAME").ok(),
            pid: std::process::id(),
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_machine_name: None,
            process_machine_id: None,
            process_machine_name: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: None,
            job_run_id: parent_run_id,
            activity_id: None,
            step_index: None,
        })
        .map_err(|error| {
            action_failed(
                action,
                format!("record pipeline.child_results audit: {error}"),
            )
        })
}

fn pipeline_wait_entry_failure(
    label: &str,
    entry: &Value,
    root_cause: Option<&RunRootCause>,
) -> Option<String> {
    let Some(status) = entry.get("status").and_then(Value::as_str) else {
        return Some(format!("{label} missing string status"));
    };
    if pipeline_wait_status_is_success(status) || pipeline_wait_status_is_held(status) {
        return None;
    }

    let error = entry
        .get("error")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    if entry.get("partition_decisions").is_some() {
        let applied = entry
            .get("applied_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let unresolved = entry
            .get("unresolved_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let counts = format!("{applied} applied, {unresolved} unresolved");
        return Some(match error {
            // The apply step's own error already carries these counts.
            Some(error) if error.contains(&counts) => {
                format!("{label} task-pilot apply status {status}: {error}")
            }
            Some(error) => format!("{label} task-pilot apply status {status}: {error} ({counts})"),
            None => format!("{label} task-pilot apply status {status} ({counts})"),
        });
    }
    let run_id = entry
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    if let Some(cause) = root_cause.filter(|cause| cause.leaf_run_id != run_id) {
        let known = |value: &Option<String>| value.clone().unwrap_or_else(|| "-".to_string());
        return Some(format!(
            "{label} run {run_id} status {status}: echoes leaf run {} (task {}, step {}, code {})",
            cause.leaf_run_id,
            known(&cause.task_id),
            known(&cause.step),
            known(&cause.code),
        ));
    }
    Some(match error {
        Some(error) => format!("{label} run {run_id} status {status}: {error}"),
        None => format!("{label} run {run_id} status {status}"),
    })
}
