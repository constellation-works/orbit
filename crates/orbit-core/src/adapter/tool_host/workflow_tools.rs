use std::collections::BTreeSet;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::identity::normalize_optional_attribution_label;
use orbit_types::workflow::{JobRun, JobRunState, PipelineState};
use serde_json::{Value, json};

use crate::application::job::{DrainWorkerLimitRequest, JobRunListParams};
use crate::runtime::run_audit::{
    MAX_RECOVERY_ATTEMPTS, RECOVERY_FETCH_PER_RUN, RunExecutionProgress, RunProviderProcess,
    RunRecoveryAttempts,
};
use crate::{OrbitRuntime, ShipMode};

use super::input::{parse_optional_string_array_field, parse_string_array_field};
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
    let allowed_crews = parse_optional_string_array_field(&input, "allowed_crews")?;
    let actor = actor(runtime, agent.as_deref(), model.as_deref());
    let claim_token = optional_string(&input, "claim_token")?;
    let invoke = runtime.submit_ship_run(
        mode,
        base.as_deref(),
        &task_ids,
        // [ORB-11187] Completion authority is an operator decision made at the
        // CLI; this tool surface does not advertise or accept it.
        crate::application::workflow::CompletionPolicy::Review,
        &allowed_crews,
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
    let run = runtime.show_job_run(&id)?;
    let mut value = run_json_with_lineage(runtime, &run)?;
    value["execution_progress"] = execution_progress_json(runtime, &run.run_id);
    Ok(value)
}

/// [ORB-11752] What the run is doing right now, for the reader that asked
/// about exactly one run.
///
/// Without it this surface can report a `running` wrapper PID and nothing
/// else, so a healthy implementation agent and an abandoned wrapper look
/// identical here and an orchestrator has to fall back to an operator command.
/// `list` deliberately does not carry it: the evidence costs an audit scan plus
/// a liveness probe per open child, which is the right price for one deliberate
/// read and the wrong one for a 200-run page.
///
/// The projection is identifiers, timestamps and process facts only — no
/// provider output — so it stays bounded and carries nothing to redact. An
/// unreadable audit trail degrades to `unavailable` rather than failing the
/// durable run read, as every other evidence field here does.
fn execution_progress_json(runtime: &OrbitRuntime, run_id: &str) -> Value {
    let progress = runtime
        .collect_run_execution_progress(run_id)
        .unwrap_or_else(|_| RunExecutionProgress::unavailable());
    json!({
        "state": progress.state,
        "active_step": progress.active_step.map(|step| json!({
            "step_id": step.step_id,
            "step_index": step.step_index,
            "started_at": step.started_at.map(|value| value.to_rfc3339()),
        })),
        "provider_processes": {
            "limit": progress.limit,
            "truncated": progress.truncated,
            "items": progress
                .provider_processes
                .iter()
                .map(RunProviderProcess::to_json)
                .collect::<Vec<_>>(),
        },
    })
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
    // Default list stays the enriched projection. A summary mode, if added,
    // must be an explicit opt-in and cannot replace this path [ORB-11625].
    let (items, _reads) = project_workflow_run_list(runtime, &runs)?;
    Ok(json!({ "items": items }))
}

/// Query counts for one list-page projection. Reconciliation reads that happen
/// inside `list_job_runs` are excluded: they are not this enrichment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunListProjectionReads {
    pub pipeline_state_queries: usize,
    pub recovery_event_queries: usize,
    pub recovery_presence_queries: usize,
    pub per_run_recovery_fetch_limit: usize,
    pub per_run_recovery_projection_limit: usize,
}

pub(crate) fn project_workflow_run_list(
    runtime: &OrbitRuntime,
    runs: &[JobRun],
) -> Result<(Vec<Value>, RunListProjectionReads), OrbitError> {
    let reads = RunListProjectionReads {
        pipeline_state_queries: usize::from(!runs.is_empty()),
        recovery_event_queries: usize::from(!runs.is_empty()),
        recovery_presence_queries: usize::from(!runs.is_empty()),
        per_run_recovery_fetch_limit: RECOVERY_FETCH_PER_RUN,
        per_run_recovery_projection_limit: MAX_RECOVERY_ATTEMPTS,
    };
    if runs.is_empty() {
        return Ok((Vec::new(), reads));
    }

    let run_ids = runs
        .iter()
        .map(|run| run.run_id.clone())
        .collect::<Vec<_>>();
    let states = runtime.read_run_states(&run_ids).unwrap_or_default();
    let recoveries = runtime
        .collect_run_recovery_attempts_for_runs(&run_ids)
        .map(|page| page.by_run_id)
        .unwrap_or_default();
    let items = runs
        .iter()
        .map(|run| {
            run_json_enriched(
                Some(runtime),
                run,
                states.get(&run.run_id).and_then(Option::as_ref),
                recoveries.get(&run.run_id),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((items, reads))
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
    if limit > MAX_RUN_LIST_LIMIT as u64 {
        return Err(OrbitError::InvalidInput(format!(
            "`limit` must be at most {MAX_RUN_LIST_LIMIT}"
        )));
    }
    usize::try_from(limit).map_err(|_| OrbitError::InvalidInput("`limit` is too large".to_string()))
}

fn run_json(run: &JobRun) -> Result<Value, OrbitError> {
    let mut value = serde_json::to_value(run).map_err(serialize_error("serialize workflow run"))?;
    value["steps"] = serde_json::to_value(&run.steps)
        .map_err(serialize_error("serialize workflow run steps"))?;
    value["steps_source"] = json!("record");
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
    let state = runtime.read_run_state(&run.run_id).ok().flatten();
    let recovery = runtime.collect_run_recovery_attempts(&run.run_id).ok();
    run_json_enriched(Some(runtime), run, state.as_ref(), recovery.as_ref())
}

fn run_json_enriched(
    runtime: Option<&OrbitRuntime>,
    run: &JobRun,
    state: Option<&PipelineState>,
    recovery: Option<&RunRecoveryAttempts>,
) -> Result<Value, OrbitError> {
    let mut value = run_json(run)?;
    if let Some(trigger) = state.and_then(|state| state.trigger.as_ref()) {
        value["trigger"] =
            serde_json::to_value(trigger).map_err(serialize_error("serialize run trigger"))?;
    }
    if let Some(runtime) = runtime {
        attach_displayed_steps(runtime, run, &mut value)?;
    }
    let dispatches = state
        .map(|state| state.child_dispatches.clone())
        .unwrap_or_default();
    value["child_dispatches"] =
        serde_json::to_value(&dispatches).map_err(serialize_error("serialize child dispatches"))?;
    // [ORB-11253] The effective ceiling and who moved it, from the same read:
    // an operator asking why a drain is admitting five tasks rather than seven
    // is asking about this field, not about the submitted input.
    value["drain_worker_limit"] =
        serde_json::to_value(state.and_then(|state| state.drain_worker_limit.as_ref()))
            .map_err(serialize_error("serialize drain worker limit"))?;
    value["drain_admissions_stop"] =
        serde_json::to_value(state.and_then(|state| state.drain_admissions_stop.as_ref()))
            .map_err(serialize_error("serialize drain admissions stop"))?;
    // [ORB-11354] An operator tracking an agent invocation reads it here, from
    // the same show/list surface as any other run: its distinguishable outcome,
    // a bounded preview of the answer, and the durable reference to the full
    // captured output.
    value["agent_invocation"] = serde_json::to_value(crate::application::job::agent_invoke_result(
        run,
        state.map(|state| &state.step_outputs),
    ))
    .map_err(serialize_error("serialize agent invocation result"))?;
    // Recovery evidence remains separate from the run and step errors above:
    // a successful or failed recovery attempt never rewrites the original
    // workflow failure that triggered it. Older/unreadable audit trails remain
    // observable as `unavailable` rather than making existing run-show callers
    // fail their ordinary durable run read.
    value["recovery_attempts"] = match recovery {
        Some(attempts) => json!({
            "state": attempts.state,
            "limit": attempts.limit,
            "truncated": attempts.truncated,
            "items": attempts.attempts.iter().map(|attempt| json!({
                "run_id": attempt.run_id,
                "event_id": attempt.event_id,
                "attempted_at": attempt.attempted_at.map(|value| value.to_rfc3339()),
                "failed_step_id": attempt.failed_step_id,
                "recovery_activity": attempt.recovery_activity,
                "outcome": attempt.outcome,
                "failure_phase": attempt.failure_phase,
                "diagnostic": attempt.diagnostic,
                "diagnostic_truncated": attempt.diagnostic_truncated,
            })).collect::<Vec<_>>(),
        }),
        None => json!({
            "state": "unavailable",
            "limit": MAX_RECOVERY_ATTEMPTS,
            "truncated": false,
            "items": [],
        }),
    };
    Ok(value)
}

/// [ORB-12255] Prefer stored `JobRun::steps`; reconstruct from the v2 audit
/// trail when the worker path left the record empty. `steps_source` names
/// which one answered, matching CLI `run show --json`.
fn attach_displayed_steps(
    runtime: &OrbitRuntime,
    run: &JobRun,
    value: &mut Value,
) -> Result<(), OrbitError> {
    if !run.steps.is_empty() {
        value["steps_source"] = json!("record");
        return Ok(());
    }
    let audit = runtime
        .collect_run_audit_steps(&run.run_id)
        .unwrap_or_default();
    if audit.is_empty() {
        value["steps_source"] = json!("record");
        return Ok(());
    }
    let steps = audit
        .iter()
        .map(|step| {
            let duration_ms = match (step.started_at, step.finished_at) {
                (Some(started), Some(finished)) => Some(
                    finished
                        .signed_duration_since(started)
                        .num_milliseconds()
                        .max(0) as u64,
                ),
                _ => None,
            };
            json!({
                "step_index": step.step_index,
                "target_type": "activity",
                "target_id": step.step_id,
                "started_at": step.started_at.map(|value| value.to_rfc3339()),
                "finished_at": step.finished_at.map(|value| value.to_rfc3339()),
                "duration_ms": duration_ms,
                "exit_code": Value::Null,
                "agent_response_json": Value::Null,
                "state": step.state.as_deref().unwrap_or("running"),
                "error_code": Value::Null,
                "error_message": step.error_message,
            })
        })
        .collect::<Vec<_>>();
    value["steps"] = Value::Array(steps);
    value["steps_source"] = json!("audit");
    Ok(())
}
