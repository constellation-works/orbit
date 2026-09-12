//! The durable dispatch checkpoint a parent run writes when it submits a
//! child Run [ORB-10971].
//!
//! `invoke_and_wait` used to be one opaque call: it invoked the child, kept
//! the returned `run_id` in a local variable, and blocked in `pipeline.wait`.
//! Because the engine does not persist an activity's output until the
//! activity returns, a parent could sit on that step for the whole wait
//! timeout — up to an hour — with no durable child identifier anywhere. An
//! operator could not tell a healthy long wait apart from a dispatch path
//! wedged before persistence, and no reader could name the child.
//!
//! This module makes the dispatch boundary fail-observable. [ORB-11310] moved
//! the first parent link into the same SQLite transaction that creates an
//! auto child, before `orbit.pipeline.invoke` returns. This checkpoint then
//! records the independent audit evidence and refreshes derived fields such as
//! `queued`. Either a durable child is already linked, or the parent fails
//! promptly carrying the exact pre-dispatch error.

use chrono::Utc;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_engine::DispatchError;
use orbit_store::contracts::AuditEventInsertParams;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{ChildDispatch, ChildDispatchPhase};
use serde_json::Value;

use crate::OrbitRuntime;

/// Audit command for a child submission attempt, successful or not.
pub(super) const CHILD_DISPATCH_AUDIT: &str = "pipeline.child_dispatch";
/// Audit command for the parent's observation of a child's terminal state.
pub(super) const CHILD_WAIT_AUDIT: &str = "pipeline.child_wait";

/// The parent step that is dispatching, as the engine named it.
pub(super) fn parent_step_id(input: &Value) -> Option<String> {
    input
        .get("step_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// The parent run id the engine injected into this activity's input.
pub(super) fn parent_run_id(input: &Value) -> Option<String> {
    input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Persist a freshly submitted child into the parent's run state.
///
/// The auto-admission path already wrote the first link atomically with the
/// child row [ORB-11310]. Re-recording is an idempotent refresh and preserves
/// the original submission timestamp. Direct callers without trusted parent
/// context retain this checkpoint as their first parent-state link.
///
/// A run state that cannot be written is a hard failure of the dispatch step,
/// not a warning. The alternative — blocking for an hour on a child nobody can
/// name — is exactly the condition this checkpoint exists to prevent. The
/// error names the child run id so an operator can still reach the work that
/// was successfully submitted.
pub(super) fn checkpoint_submitted_child(
    runtime: &OrbitRuntime,
    action: &str,
    parent_run_id: Option<&str>,
    dispatch: &ChildDispatch,
) -> Result<(), DispatchError> {
    record_child_audit(
        runtime,
        action,
        CHILD_DISPATCH_AUDIT,
        AuditEventStatus::Success,
        parent_run_id,
        Some(&dispatch.child_run_id),
        serde_json::json!({
            "parent_run_id": parent_run_id,
            "parent_step_id": dispatch.parent_step_id,
            "child_run_id": dispatch.child_run_id,
            "job_name": dispatch.job_name,
            "action": dispatch.action,
            "blocking": dispatch.blocking,
            "queued": dispatch.queued,
            "phase": dispatch.phase.as_str(),
            "submitted_at": dispatch.submitted_at.to_rfc3339(),
        }),
        None,
    )?;

    let Some(parent_run_id) = parent_run_id else {
        // A direct `execute_job` caller with no persisted run has no state to
        // checkpoint into. The audit event above is then the only lineage
        // record, which is the same contract `checkpoint_step` follows.
        return Ok(());
    };

    // [ORB-11253] One transaction for the read and the write, so this
    // checkpoint cannot discard a run control an operator set on the same
    // parent state between them.
    runtime
        .stores()
        .jobs()
        .update_run_state(parent_run_id, &mut |_, state| {
            state.record_child_dispatch(dispatch.clone());
            Ok(())
        })
        .map(|_| ())
        .map_err(|error| {
            action_failed(
                action,
                format!(
                    "child run '{}' (job '{}') was submitted but its dispatch checkpoint could not \
                     be persisted to parent run '{parent_run_id}': {error}",
                    dispatch.child_run_id, dispatch.job_name
                ),
            )
        })
}

/// Move a recorded dispatch to a new phase.
///
/// Non-fatal by contract, unlike the submission checkpoint: by the time this
/// runs the child is already durably linked, so a failed projection update
/// must not discard a child result the parent did observe. Failures are
/// traced and the caller continues.
pub(super) fn advance_child_phase(
    runtime: &OrbitRuntime,
    parent_run_id: Option<&str>,
    child_run_id: &str,
    phase: ChildDispatchPhase,
    child_status: Option<String>,
    error: Option<String>,
) {
    let Some(parent_run_id) = parent_run_id else {
        return;
    };
    // Keep phase bookkeeping on the same transactional update path as stop
    // and admission. A read followed by a plain write here could otherwise
    // restore a stale pre-stop document after the operator was acknowledged.
    let update = runtime
        .stores()
        .jobs()
        .update_run_state(parent_run_id, &mut |_, state| {
            state.advance_child_dispatch(child_run_id, phase, child_status.clone(), error.clone());
            Ok(())
        });
    if let Err(error) = update {
        tracing::warn!(
            parent_run_id,
            child_run_id,
            phase = phase.as_str(),
            %error,
            "could not persist child dispatch phase"
        );
    }
}

/// Record that the parent observed the child's terminal state.
pub(super) fn record_child_wait_outcome(
    runtime: &OrbitRuntime,
    action: &str,
    parent_run_id: Option<&str>,
    child_run_id: &str,
    status: &str,
    error_message: Option<&str>,
) -> Result<(), DispatchError> {
    let succeeded = crate::application::job::pipeline::pipeline_wait_status_is_success(status);
    record_child_audit(
        runtime,
        action,
        CHILD_WAIT_AUDIT,
        if succeeded {
            AuditEventStatus::Success
        } else {
            AuditEventStatus::Failure
        },
        parent_run_id,
        Some(child_run_id),
        serde_json::json!({
            "parent_run_id": parent_run_id,
            "child_run_id": child_run_id,
            "status": status,
            "error": error_message,
        }),
        error_message,
    )
}

/// Record a submission that never produced a durable child run.
///
/// The concrete `orbit.pipeline.invoke` error is the diagnosis: capacity,
/// dependency, and lock waits all look identical from outside without it.
pub(super) fn record_dispatch_failure(
    runtime: &OrbitRuntime,
    action: &str,
    parent_run_id: Option<&str>,
    parent_step_id: Option<&str>,
    job_name: &str,
    error_message: &str,
) {
    if let Err(error) = record_child_audit(
        runtime,
        action,
        CHILD_DISPATCH_AUDIT,
        AuditEventStatus::Failure,
        parent_run_id,
        None,
        serde_json::json!({
            "parent_run_id": parent_run_id,
            "parent_step_id": parent_step_id,
            "job_name": job_name,
            "action": action,
            "error": error_message,
        }),
        Some(error_message),
    ) {
        tracing::warn!(%error, job_name, "could not record child dispatch failure audit");
    }
}

#[allow(clippy::too_many_arguments)]
fn record_child_audit(
    runtime: &OrbitRuntime,
    action: &str,
    command: &str,
    status: AuditEventStatus,
    parent_run_id: Option<&str>,
    child_run_id: Option<&str>,
    payload: Value,
    error_message: Option<&str>,
) -> Result<(), DispatchError> {
    let arguments_json = serde_json::to_string(&payload)
        .map_err(|error| action_failed(action, format!("serialize {command} payload: {error}")))?;
    runtime
        .record_audit_event(&AuditEventInsertParams {
            execution_id: audit_execution_id("audit-child-dispatch"),
            command: command.to_string(),
            subcommand: None,
            tool_name: None,
            target_type: Some("job_run".to_string()),
            target_id: child_run_id.map(ToOwned::to_owned),
            role: "admin".to_string(),
            status,
            exit_code: i32::from(status != AuditEventStatus::Success),
            duration_ms: 0,
            working_directory: runtime.paths().repo_root.to_string_lossy().into_owned(),
            arguments_json: Some(arguments_json),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: error_message.map(ToOwned::to_owned),
            host: std::env::var("HOSTNAME").ok(),
            pid: std::process::id(),
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_host_id: None,
            process_machine_id: None,
            process_host_id: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: None,
            job_run_id: parent_run_id.map(ToOwned::to_owned),
            activity_id: None,
            step_index: None,
        })
        .map_err(|error| action_failed(action, format!("record {command} audit: {error}")))
}

/// Build the dispatch record for a child `orbit.pipeline.invoke` just returned.
pub(super) fn dispatch_from_invoke_output(
    action: &str,
    job_name: &str,
    blocking: bool,
    parent_step_id: Option<String>,
    invoke_output: &Value,
) -> Result<ChildDispatch, DispatchError> {
    let child_run_id = invoke_output
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| action_failed(action, "pipeline.invoke returned no run_id".to_string()))?
        .to_string();
    let submitted_at = invoke_output
        .get("submitted_at")
        .and_then(Value::as_str)
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);

    Ok(ChildDispatch::submitted(
        child_run_id,
        job_name.to_string(),
        action.to_string(),
        blocking,
        invoke_output
            .get("queued")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        submitted_at,
    )
    .with_parent_step_id(parent_step_id))
}

fn action_failed(action: &str, message: String) -> DispatchError {
    DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message,
    }
}
