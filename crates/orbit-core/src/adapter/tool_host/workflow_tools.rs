use std::collections::BTreeSet;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::identity::normalize_optional_attribution_label;
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};

use crate::application::job::{DrainWorkerLimitRequest, JobRunListParams};
use crate::{OrbitRuntime, ShipMode};

use super::input::parse_string_array_field;
use super::json::serialize_error;

const DEFAULT_RUN_LIST_LIMIT: usize = 25;
const MAX_RUN_LIST_LIMIT: usize = 200;

pub(super) fn ship(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let task_ids = parse_string_array_field(&input, "task_ids")?;
    let unique = task_ids.iter().collect::<BTreeSet<_>>();
    if unique.len() != task_ids.len() {
        return Err(OrbitError::InvalidInput(
            "`task_ids` must not contain duplicates".to_string(),
        ));
    }
    let mode = match optional_string(&input, "mode")? {
        Some(raw) => ShipMode::parse(&raw)?,
        None => runtime
            .workspace_runtime_binding()
            .map_or(ShipMode::Pr, |binding| binding.ship_mode),
    };
    let base = optional_string(&input, "base")?;
    let actor = actor(runtime, agent.as_deref(), model.as_deref());
    let claim_token = optional_string(&input, "claim_token")?;
    let invoke = runtime.submit_ship_run(
        mode,
        base.as_deref(),
        &task_ids,
        // [ORB-11187] Completion authority is an operator decision made at the
        // CLI; this tool surface does not advertise or accept it.
        crate::application::workflow::CompletionPolicy::Review,
        Some(&actor),
        claim_token.as_deref(),
    )?;
    Ok(json!({
        "workflow": "ship",
        "job_id": invoke.job_name,
        "run_id": invoke.run_id,
        "state": if invoke.queued { "queued" } else { "submitted" },
        "submitted_at": invoke.submitted_at,
    }))
}

pub(super) fn show(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let id = orbit_common::protocol::tool_input::required_string(&input, &["id"], "id")?;
    run_json_with_lineage(runtime, &runtime.show_job_run(&id)?)
}

pub(super) fn list(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let state = optional_string(&input, "state")?;
    let terminal_only = state.as_deref() == Some("terminal");
    let state = state
        .filter(|value| value != "terminal")
        .map(|value| {
            JobRunState::from_str(&value)
                .map_err(|error| OrbitError::InvalidInput(format!("`state` {error}")))
        })
        .transpose()?;
    let since = optional_string(&input, "since")?
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|value| value.with_timezone(&Utc))
                .map_err(|error| OrbitError::InvalidInput(format!("`since` {error}")))
        })
        .transpose()?;
    let runs = runtime.list_job_runs(JobRunListParams {
        job_id: optional_string(&input, "job_id")?,
        state,
        terminal_only,
        since,
        limit: Some(parse_limit(&input)?),
        ..Default::default()
    })?;
    let items = runs
        .iter()
        .map(|run| run_json_with_lineage(runtime, run))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({ "items": items }))
}

pub(super) fn resume(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let id = orbit_common::protocol::tool_input::required_string(&input, &["id"], "id")?;
    let actor = actor(runtime, agent.as_deref(), model.as_deref());
    let claim_token = optional_string(&input, "claim_token")?;
    let invoke = runtime.submit_resume_run(&id, Some(&actor), claim_token.as_deref())?;
    Ok(json!({
        "workflow": "resume",
        "job_id": invoke.job_name,
        "run_id": invoke.run_id,
        "retry_source_run_id": id,
        "state": if invoke.queued { "queued" } else { "submitted" },
        "submitted_at": invoke.submitted_at,
    }))
}

/// [ORB-11253] Move a live drain's worker ceiling without replacing its run.
pub(super) fn workers(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let id = orbit_common::protocol::tool_input::required_string(&input, &["id"], "id")?;
    let concurrency = required_u32(&input, "concurrency")?;
    let expected_revision = optional_u32(&input, "if_revision")?;
    let reason = optional_string(&input, "reason")?;
    let claim_token = optional_string(&input, "claim_token")?;
    let actor = actor(runtime, agent.as_deref(), model.as_deref());
    let change = runtime.set_drain_worker_limit(DrainWorkerLimitRequest {
        run_id: &id,
        max_active_leaf_runs: concurrency,
        expected_revision,
        reason: reason.as_deref(),
        actor: &actor,
        source: "tool",
        claim_token: claim_token.as_deref(),
    })?;
    Ok(json!({
        "run_id": change.run_id,
        "job_id": change.job_id,
        "outcome": change.outcome,
        "previous_concurrency": change.previous_max_active_leaf_runs,
        "concurrency": change.max_active_leaf_runs,
        "revision": change.revision,
        "hard_limit": change.hard_limit,
    }))
}

fn required_u32(input: &Value, field: &str) -> Result<u32, OrbitError> {
    optional_u32(input, field)?
        .ok_or_else(|| OrbitError::InvalidInput(format!("`{field}` is required")))
}

fn optional_u32(input: &Value, field: &str) -> Result<Option<u32>, OrbitError> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!("`{field}` must be a non-negative integer"))
            }),
    }
}

fn optional_string(input: &Value, field: &str) -> Result<Option<String>, OrbitError> {
    orbit_common::protocol::tool_input::optional_string(input, field)
}

fn actor(runtime: &OrbitRuntime, agent: Option<&str>, model: Option<&str>) -> String {
    normalize_optional_attribution_label(model.or(agent), model)
        .unwrap_or_else(|| runtime.actor_label().to_string())
}

fn parse_limit(input: &Value) -> Result<usize, OrbitError> {
    let Some(value) = input.get("limit") else {
        return Ok(DEFAULT_RUN_LIST_LIMIT);
    };
    let limit = value.as_u64().ok_or_else(|| {
        OrbitError::InvalidInput("`limit` must be a positive integer".to_string())
    })?;
    if limit == 0 {
        return Err(OrbitError::InvalidInput(
            "`limit` must be at least 1".to_string(),
        ));
    }
    usize::try_from(limit.min(MAX_RUN_LIST_LIMIT as u64))
        .map_err(|_| OrbitError::InvalidInput("`limit` is too large".to_string()))
}

fn run_json(run: &JobRun) -> Result<Value, OrbitError> {
    let mut value = serde_json::to_value(run).map_err(serialize_error("serialize workflow run"))?;
    value["steps"] = serde_json::to_value(&run.steps)
        .map_err(serialize_error("serialize workflow run steps"))?;
    Ok(value)
}

/// [ORB-10971] The run record plus the child Runs it dispatched.
///
/// The `JobRun` row alone cannot answer "what did this run submit, and is it
/// still waiting on it" — that lives in the run's `PipelineState`. Reading it
/// here is what keeps the MCP surface agreeing with the CLI and the dashboard
/// about lineage instead of each reader seeing a different half of the truth.
/// An unreadable state degrades to an empty list rather than failing the read.
fn run_json_with_lineage(runtime: &OrbitRuntime, run: &JobRun) -> Result<Value, OrbitError> {
    let mut value = run_json(run)?;
    let state = runtime.read_run_state(&run.run_id).ok().flatten();
    let dispatches = state
        .as_ref()
        .map(|state| state.child_dispatches.clone())
        .unwrap_or_default();
    value["child_dispatches"] =
        serde_json::to_value(&dispatches).map_err(serialize_error("serialize child dispatches"))?;
    // [ORB-11253] The effective ceiling and who moved it, from the same read:
    // an operator asking why a drain is admitting five tasks rather than seven
    // is asking about this field, not about the submitted input.
    value["drain_worker_limit"] = serde_json::to_value(
        state
            .as_ref()
            .and_then(|state| state.drain_worker_limit.as_ref()),
    )
    .map_err(serialize_error("serialize drain worker limit"))?;
    value["drain_admissions_stop"] = serde_json::to_value(
        state
            .as_ref()
            .and_then(|state| state.drain_admissions_stop.as_ref()),
    )
    .map_err(serialize_error("serialize drain admissions stop"))?;
    // [ORB-11354] An operator tracking an agent invocation reads it here, from
    // the same show/list surface as any other run: its distinguishable outcome,
    // a bounded preview of the answer, and the durable reference to the full
    // captured output.
    value["agent_invocation"] = serde_json::to_value(crate::application::job::agent_invoke_result(
        run,
        state.as_ref().map(|state| &state.step_outputs),
    ))
    .map_err(serialize_error("serialize agent invocation result"))?;
    Ok(value)
}
