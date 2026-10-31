//! `orbit.agent.invoke` protocol translation [ORB-11354].
//!
//! Parses the tool payload and hands it to
//! [`OrbitRuntime::submit_agent_invocation`], which owns the admission and
//! every validation rule for both modes — an operator's trusted-host
//! invocation and a managed run's read-only explorer [ORB-16814]. Nothing
//! here decides anything.
//!
//! A sandboxed agent's explorer submission is brokered to the run's host
//! worker [ORB-20566], so the explorer's own sandbox is created beside the
//! caller's rather than nested inside it, where the kernel refuses it.

use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{optional_string, required_string};
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::{
    AgentInvocation, AgentInvokeRequest, AgentInvokeSubmission, ExploreSubmission,
};
use crate::runtime::authorization::ManagedRunCaller;

pub(super) fn invoke(
    runtime: &OrbitRuntime,
    session_context: &ToolSessionContext,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let prompt = required_string(&input, &["prompt"], "prompt")?;
    let cwd = required_string(&input, &["cwd"], "cwd")?;
    let crew = optional_string(&input, "crew")?;
    let idempotency_key = optional_string(&input, "idempotency_key")?;
    let provider_sandbox = optional_string(&input, "provider_sandbox")?;
    let timeout_seconds = parse_timeout(&input)?;
    // Validate before admission creates a durable run, even for an asynchronous call.
    let wait_seconds = parse_wait_seconds(&input)?;
    let actor = orbit_types::identity::normalize_optional_attribution_label(
        model.as_deref().or(agent.as_deref()),
        model.as_deref(),
    );

    let submission = runtime.submit_agent_invocation(AgentInvokeRequest {
        prompt: &prompt,
        cwd: &cwd,
        crew: crew.as_deref(),
        timeout_seconds,
        idempotency_key: idempotency_key.as_deref(),
        provider_sandbox: provider_sandbox.as_deref(),
        actor: actor.as_deref(),
        session_context,
    })?;
    match submission {
        AgentInvocation::TrustedHost(submission) => {
            trusted_host_response(runtime, submission, wait_seconds)
        }
        AgentInvocation::Explore(submission) => explore_response(runtime, submission, wait_seconds),
    }
}

fn trusted_host_response(
    runtime: &OrbitRuntime,
    submission: AgentInvokeSubmission,
    wait_seconds: Option<u64>,
) -> Result<Value, OrbitError> {
    let mut response = submission.tool_submission_json(wait_seconds.is_some());
    if let Some(seconds) = wait_seconds {
        let result = runtime.wait_agent_invoke_run(&submission.run_id, Some(seconds))?;
        result.project_submission_result(&mut response)?;
    }
    Ok(response)
}

fn explore_response(
    runtime: &OrbitRuntime,
    submission: ExploreSubmission,
    wait_seconds: Option<u64>,
) -> Result<Value, OrbitError> {
    let response = json!({
        "run_id": submission.run_id,
        "job_id": submission.job_id,
        "submitted_at": submission.submitted_at,
        "state": submission.state,
        "queued": submission.queued,
        "queue_position": submission.queue_position,
        "waited": false,
        "deduplicated": submission.deduplicated,
        "timeout_seconds": submission.timeout_seconds,
        "parent_run_id": submission.parent_run_id,
        "crew": submission.crew,
        "eligible_pool": submission.eligible_pool,
        "workspace_path": submission.workspace_path,
        "cwd": submission.cwd,
        "sandboxed": true,
        "read_only": true,
        "calls_used": submission.calls_used,
        "calls_allowed": submission.calls_allowed,
        "max_answer_bytes": submission.max_answer_bytes,
    });
    match wait_seconds {
        Some(seconds) => wait_for_explorer_answer(runtime, response, seconds),
        None => Ok(response),
    }
}

/// Submit the explorer a sandboxed agent of `caller` asked its run's broker
/// for [ORB-20566]. The caller is the run the host dispatched, never one named
/// by the request or this process's environment. Waiting stays with the
/// agent's own process ([`wait_for_explorer_answer`]), so a long wait holds no
/// broker connection.
pub(crate) fn invoke_brokered_explorer(
    runtime: &OrbitRuntime,
    caller: ManagedRunCaller,
    input: &Value,
    actor: Option<&str>,
) -> Result<Value, OrbitError> {
    let prompt = required_string(input, &["prompt"], "prompt")?;
    let cwd = required_string(input, &["cwd"], "cwd")?;
    let crew = optional_string(input, "crew")?;
    let idempotency_key = optional_string(input, "idempotency_key")?;
    let provider_sandbox = optional_string(input, "provider_sandbox")?;
    let timeout_seconds = parse_timeout(input)?;
    if parse_wait_seconds(input)?.is_some() {
        return Err(OrbitError::InvalidInput(
            "a brokered explorer submission does not wait; the caller waits for the answer"
                .to_string(),
        ));
    }
    let session_context = ToolSessionContext::default();
    let submission = runtime.submit_explore_run(
        caller,
        AgentInvokeRequest {
            prompt: &prompt,
            cwd: &cwd,
            crew: crew.as_deref(),
            timeout_seconds,
            idempotency_key: idempotency_key.as_deref(),
            provider_sandbox: provider_sandbox.as_deref(),
            actor,
            session_context: &session_context,
        },
    )?;
    explore_response(runtime, submission, None)
}

/// Validate a sandboxed agent's `wait_seconds` before its explorer is
/// submitted through the broker, so an invalid wait creates no run.
pub(crate) fn brokered_explorer_wait_seconds(input: &Value) -> Result<Option<u64>, OrbitError> {
    parse_wait_seconds(input)
}

/// Wait up to `seconds` for the explorer `response` names and project its
/// bounded answer into the response.
pub(crate) fn wait_for_explorer_answer(
    runtime: &OrbitRuntime,
    mut response: Value,
    seconds: u64,
) -> Result<Value, OrbitError> {
    let run_id = response["run_id"]
        .as_str()
        .ok_or_else(|| OrbitError::Execution("explorer submission names no run".to_string()))?
        .to_string();
    let result = runtime.wait_agent_invoke_run(&run_id, Some(seconds))?;
    response["waited"] = json!(true);
    response["state"] = json!(result.outcome);
    let bounded = runtime.bound_explore_answer(result);
    response["answer_truncated"] = json!(bounded.answer_truncated);
    let result = serde_json::to_value(bounded.result)
        .map_err(|error| OrbitError::Execution(format!("encode explorer answer: {error}")))?;
    response["answer"] = result["answer"].clone();
    response["agent_invocation"] = result;
    Ok(response)
}

fn parse_wait_seconds(input: &Value) -> Result<Option<u64>, OrbitError> {
    match input.get("wait_seconds") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|seconds| *seconds <= 600)
            .map(Some)
            .ok_or_else(|| {
                OrbitError::InvalidInput(
                    "`wait_seconds` must be an integer from 0 to 600".to_string(),
                )
            }),
    }
}

fn parse_timeout(input: &Value) -> Result<Option<u64>, OrbitError> {
    match input.get("timeout_seconds") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            OrbitError::InvalidInput("`timeout_seconds` must be a non-negative integer".to_string())
        }),
    }
}
