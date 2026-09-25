//! Pipeline success guard and its terminal-result audit.

use orbit_common::observability::audit_id::audit_execution_id;
use orbit_engine::DispatchError;
use orbit_store::contracts::AuditEventInsertParams;
use orbit_types::telemetry::AuditEventStatus;
use serde_json::Value;

use crate::OrbitRuntime;

use super::action_failed;

pub(in super::super) fn pipeline_success_guard(
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
    let mut checked_count = 0usize;
    let mut failures = Vec::new();

    if let Some(result) = input.get("result")
        && !result.is_null()
    {
        checked_count += 1;
        if let Some(failure) = pipeline_wait_entry_failure("result", result) {
            failures.push(failure);
        }
    }

    if let Some(results) = input.get("results")
        && !results.is_null()
    {
        let entries =
            results
                .as_array()
                .ok_or_else(|| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: "`results` must be an array".to_string(),
                })?;
        for (idx, entry) in entries.iter().enumerate() {
            checked_count += 1;
            if let Some(failure) = pipeline_wait_entry_failure(&format!("results[{idx}]"), entry) {
                failures.push(failure);
            }
        }
    }

    if checked_count == 0 {
        return Err(DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: "expected `result` or `results` to check".to_string(),
        });
    }

    if !failures.is_empty() {
        return Err(DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("{context} did not succeed: {}", failures.join("; ")),
        });
    }

    Ok(serde_json::json!({
        "succeeded": true,
        "checked_count": checked_count,
    }))
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
            status
                if crate::application::job::pipeline::pipeline_wait_status_is_success(status) =>
            {
                succeeded_count += 1
            }
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

fn pipeline_wait_entry_failure(label: &str, entry: &Value) -> Option<String> {
    let Some(status) = entry.get("status").and_then(Value::as_str) else {
        return Some(format!("{label} missing string status"));
    };
    if crate::application::job::pipeline::pipeline_wait_status_is_success(status) {
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
        return Some(match error {
            Some(error) => format!(
                "{label} task-pilot apply status {status}: {error} ({applied} applied, {unresolved} unresolved)"
            ),
            None => format!(
                "{label} task-pilot apply status {status} ({applied} applied, {unresolved} unresolved)"
            ),
        });
    }
    let run_id = entry
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    Some(match error {
        Some(error) => format!("{label} run {run_id} status {status}: {error}"),
        None => format!("{label} run {run_id} status {status}"),
    })
}
