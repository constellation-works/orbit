//! Bounded desktop projections. Reads never reconcile or dispatch a run.
use std::str::FromStr;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{
    optional_csv_or_string_list_alias, optional_string, required_string,
};
use orbit_common::security::redaction::redact_all;
use orbit_store::contracts::{JobRunQuery, TaskListFilter};
use orbit_types::task::{TaskEnvelopeV2, TaskPriority, TaskRelationType, TaskStatus};
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::runtime::audit::run::{RunExecutionProgress, RunProviderProcess};

const MAX_PAGE: usize = 50;
const MAX_RUN_PREFIX: usize = 10_000;
const MAX_LOG_PREFIX: usize = 200;
const LOG_BYTES: usize = 4096;

pub(super) fn read(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    input: Value,
) -> Result<Value, OrbitError> {
    let workspace = required_string(&input, &["workspace"], "workspace")?;
    let scope = required_string(&input, &["scope"], "scope")?;
    let limit = number(&input, "limit", 25)?;
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(invalid("limit must be between 1 and 50"));
    }
    let mut result = match scope.as_str() {
        "tasks" => tasks(runtime, &input, limit)?,
        "task" => task(runtime, session, &input, limit)?,
        "runs" => runs(runtime, &input, limit)?,
        "drain" => super::drain::readiness(runtime)?,
        "routines" | "auto_tasks" | "jobs" => {
            super::automation::read(runtime, &scope, number(&input, "offset", 0)?, limit)?
        }
        "run" => run(runtime, &input, limit)?,
        _ => {
            return Err(invalid(
                "scope must be tasks, task, runs, run, drain, routines, auto_tasks or jobs",
            ));
        }
    };
    result["schema_version"] = json!(1);
    result["workspace"] = json!(workspace);
    result["scope"] = json!(scope);
    if result.get("observed_at").is_none() {
        result["observed_at"] = json!(Utc::now().to_rfc3339());
    }
    Ok(result)
}

fn tasks(runtime: &OrbitRuntime, input: &Value, limit: usize) -> Result<Value, OrbitError> {
    let offset = number(input, "offset", 0)?;
    let statuses = match input.get("status") {
        None | Some(Value::Null) => None,
        _ => optional_csv_or_string_list_alias(input, &["status"])?,
    };
    let mut filter = TaskListFilter {
        search: optional_string(input, "search")?,
        statuses: statuses
            .map(|values| {
                values
                    .into_iter()
                    .map(|value| TaskStatus::from_str(&value).map_err(invalid))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .filter(|values| !values.is_empty()),
        priority: optional_string(input, "priority")?
            .map(|v| TaskPriority::from_str(&v).map_err(invalid))
            .transpose()?,
        ..TaskListFilter::default()
    };
    // Walk metadata only, with a stable cursor instead of hydrating skipped bodies.
    let mut remaining = offset;
    let mut items = Vec::new();
    let mut total = None;
    loop {
        let count = remaining.saturating_add(limit - items.len()).min(200);
        let page = runtime.task_candidates(&filter, count)?;
        total.get_or_insert(page.total_without_cursor);
        if page.items.is_empty() {
            break;
        }
        if let Some(last) = page.items.last() {
            filter.scan_before = Some((last.created_at, last.id.to_string()));
        }
        for task in page.items {
            if remaining > 0 {
                remaining -= 1;
                continue;
            }
            items.push(task_list_item(&task, runtime.automation_machine_identity()));
        }
        if items.len() == limit {
            break;
        }
    }
    let total = total.unwrap_or(0);
    Ok(
        json!({"pagination": pagination(offset, limit, items.len(), total, usize::MAX),
        "items": items, "total": total, "search_scope": "task key and title"}),
    )
}

fn task_list_item(task: &TaskEnvelopeV2, local_machine: Option<&str>) -> Value {
    let (title, title_truncated) = bounded_text(&task.title, 512);
    let (crew, crew_truncated) = task
        .crew
        .as_deref()
        .map(|value| {
            let (value, truncated) = bounded_text(value, 128);
            (Some(value), truncated)
        })
        .unwrap_or((None, false));
    // Never turn an oversized relation identity into a different selectable ID.
    let relations = task
        .relations
        .iter()
        .take(MAX_PAGE)
        .filter(|relation| relation.target.len() <= 512)
        .collect::<Vec<_>>();
    let relations_truncated = relations.len() < task.relations.len();
    let dependencies = relations
        .iter()
        .filter(|relation| relation.relation_type == TaskRelationType::BlockedBy)
        .map(|relation| &relation.target)
        .collect::<Vec<_>>();
    let dependencies_total = task
        .relations
        .iter()
        .filter(|relation| relation.relation_type == TaskRelationType::BlockedBy)
        .count();
    let job_run_id = task.job_run_id.as_deref().filter(|id| id.len() <= 2048);
    json!({
        "id":task.id,"title":title,"title_truncated":title_truncated,
        "status":task.status,"priority":task.priority,"crew":crew,"crew_truncated":crew_truncated,
        "relations":relations,"relations_total":task.relations.len(),"relations_truncated":relations_truncated,
        "dependencies":dependencies,"dependencies_total":dependencies_total,"dependencies_truncated":dependencies.len() < dependencies_total,
        "job_run_machine":task.job_run_machine.as_ref().map(|host| json!({
            "machine_id":bounded_text(&host.machine_id,512).0,
            "machine_name":host.machine_name.as_deref().map(|name| bounded_text(name,128).0),
        })),
        "job_run_navigable":job_run_id.is_some() && task.job_run_machine.as_ref().is_none_or(|host| local_machine == Some(host.machine_id.as_str())),
        "job_run_id":job_run_id,"job_run_id_omitted":task.job_run_id.is_some() && job_run_id.is_none(),
        "created_at":task.created_at,"updated_at":task.updated_at,
    })
}

fn task(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    input: &Value,
    limit: usize,
) -> Result<Value, OrbitError> {
    let id = required_string(input, &["id"], "id")?;
    let comments = number(input, "comments_offset", 0)?;
    let history = number(input, "history_offset", 0)?;
    let artifacts = number(input, "artifacts_offset", 0)?;
    let snapshot =
        runtime.desktop_task_snapshot_page(&id, comments, history, artifacts, limit, session)?;
    let mut value = serde_json::to_value(snapshot)
        .map_err(super::super::json::serialize_error("desktop task"))?;
    for (field, offset) in [
        ("comments", comments),
        ("history", history),
        ("artifacts", artifacts),
    ] {
        let total = value[format!("{field}_total")].as_u64().unwrap_or(0) as usize;
        // Oversized opaque metadata entries may be omitted by the snapshot.
        // Advance across consumed storage rows, not just visible rows.
        let consumed = total.saturating_sub(offset).min(limit);
        value[format!("{field}_pagination")] =
            pagination(offset, limit, consumed, total, usize::MAX);
    }
    Ok(value)
}

fn runs(runtime: &OrbitRuntime, input: &Value, limit: usize) -> Result<Value, OrbitError> {
    let offset = number(input, "offset", 0)?;
    prefix_bound(offset, MAX_RUN_PREFIX, "offset")?;
    let query = JobRunQuery {
        state: optional_string(input, "status")?
            .map(|s| JobRunState::from_str(&s).map_err(invalid))
            .transpose()?,
        limit: Some(offset.saturating_add(limit).min(MAX_RUN_PREFIX)),
        include_steps: false,
        ..JobRunQuery::default()
    };
    let total = runtime.stores().jobs().count_job_runs_filtered(&query)? as usize;
    let items = runtime
        .stores()
        .jobs()
        .list_job_runs_filtered(&query)?
        .iter()
        .skip(offset)
        .map(run_summary)
        .collect::<Vec<_>>();
    Ok(
        json!({"pagination": pagination(offset, limit, items.len(), total, MAX_RUN_PREFIX),
        "items": items, "total": total, "available_through": MAX_RUN_PREFIX.min(total)}),
    )
}

fn run_summary(run: &JobRun) -> Value {
    json!({"id": run.run_id, "run_id": run.run_id, "job_id": run.job_id,
        "state": run.state, "attempt": run.attempt, "scheduled_at": run.scheduled_at,
        "started_at": run.started_at, "finished_at": run.finished_at,
        "created_at": run.created_at, "duration_ms": run.duration_ms,
        "usage": {"state": "unavailable"}})
}

fn run(runtime: &OrbitRuntime, input: &Value, limit: usize) -> Result<Value, OrbitError> {
    let id = required_string(input, &["id"], "id")?;
    let offset = number(input, "log_offset", 0)?;
    prefix_bound(offset, MAX_LOG_PREFIX, "log_offset")?;
    let run = runtime.show_job_run_observed(&id)?;
    let mut value = run_summary(&run);
    let state = runtime.read_run_state(&id).ok().flatten();
    // Provider responses and original inputs are deliberately absent.
    value["steps"] =
        json!(run.steps.iter().take(MAX_PAGE).map(|s| json!({
        "step_index": s.step_index, "target_id": s.target_id, "target_type": s.target_type,
        "state": s.state, "started_at": s.started_at, "finished_at": s.finished_at,
        "duration_ms": s.duration_ms, "exit_code": s.exit_code, "error_code": s.error_code,
        "error_message": s.error_message.as_deref().map(redact_all),
    })).collect::<Vec<_>>());
    value["steps_total"] = json!(run.steps.len());
    value["steps_truncated"] = json!(run.steps.len() > MAX_PAGE);
    let progress = runtime
        .collect_run_execution_progress(&id)
        .unwrap_or_else(|_| RunExecutionProgress::unavailable());
    value["agent_invocation"] = serde_json::to_value(crate::application::job::agent_invoke_result(
        &run,
        state.as_ref().map(|state| &state.step_outputs),
        progress.provider_processes.last(),
    ))
    .map_err(super::super::json::serialize_error(
        "serialize agent invocation result",
    ))?;
    let execution_progress = json!({"state": progress.state,
        "active_step": progress.active_step.map(|s| json!({"step_id": s.step_id,"step_index":s.step_index,"started_at":s.started_at})),
        "provider_processes": {"limit":progress.limit,"truncated":progress.truncated,
            "items":progress.provider_processes.iter().map(RunProviderProcess::to_json).collect::<Vec<_>>()}});
    let audit_events = runtime.collect_run_audit_events(&id)?;
    let log_state = if audit_events.is_empty() {
        "unavailable"
    } else {
        "observed"
    };
    let total = audit_events
        .iter()
        .filter(|event| event.body_kind.as_deref() == Some("cli_invocation_finished"))
        .count();
    let logs = runtime.collect_run_cli_invocations_bounded(&id, Some(offset.saturating_add(limit).min(MAX_LOG_PREFIX)), Some(LOG_BYTES))?
        .into_iter().skip(offset).map(|r| {
            let (stdout, stdout_truncated) = excerpt(&r.stdout);
            let (stderr, stderr_truncated) = excerpt(&r.stderr);
            json!({
            "event_id":r.event_id,"ts":r.ts,"step_id":r.step_id,"step_index":r.step_index,
            "provider":r.provider,"stdout":stdout,"stderr":stderr,
            "stdout_truncated":r.stdout_blob_truncated || stdout_truncated,"stderr_truncated":r.stderr_blob_truncated || stderr_truncated,
            "exit_code":r.exit_code,"timed_out":r.timed_out,"duration_ms":r.duration_ms,
        })}).collect::<Vec<_>>();
    Ok(
        json!({"run": value, "usage":{"state":"unavailable"}, "execution_progress":execution_progress,
        "logs":{"state":log_state,"pagination":pagination(offset,limit,logs.len(),total,MAX_LOG_PREFIX),
            "items":logs,"total":total,"available_through":MAX_LOG_PREFIX.min(total),"excerpt_max_bytes":LOG_BYTES}}),
    )
}

fn pagination(offset: usize, limit: usize, returned: usize, total: usize, cap: usize) -> Value {
    let next = offset.saturating_add(returned);
    let more = next < total;
    json!({"offset":offset,"limit":limit,"truncated":more,
        "next_offset":if more && returned > 0 && next < cap {Some(next)} else {None}})
}

fn number(input: &Value, name: &str, default: usize) -> Result<usize, OrbitError> {
    match input.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| invalid(format!("{name} must be a nonnegative integer"))),
    }
}

fn prefix_bound(offset: usize, maximum: usize, field: &str) -> Result<(), OrbitError> {
    if offset >= maximum {
        return Err(invalid(format!("{field} must be less than {maximum}")));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> OrbitError {
    OrbitError::InvalidInput(message.into())
}

fn excerpt(raw: &str) -> (String, bool) {
    bounded_text(raw, LOG_BYTES)
}

pub(super) fn bounded_text(raw: &str, max_bytes: usize) -> (String, bool) {
    let mut text = redact_all(raw);
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}
