//! Bounded desktop projections. Reads never reconcile or dispatch a run.
use std::str::FromStr;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{
    optional_csv_or_string_list_alias, optional_string, required_string,
};
use orbit_store::contracts::{JobRunQuery, TaskListFilter};
use orbit_types::task::{TaskEnvelopeV2, TaskPriority, TaskRelationType, TaskStatus};
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::{
    JobRunProjectionEvidence, JobRunProjectionProfile, RunStepSource,
    add_bounded_display_steps_to_json, bound_bounded_agent_invocation_answer, project_job_run,
    run_display_steps, run_execution_progress_to_json,
};
use crate::runtime::audit::run_logs::{
    BOUNDED_EXCERPT_MAX_BYTES, RunCliInvocationProfile, bounded_redacted_text,
};

const MAX_PAGE: usize = 50;
const MAX_RUN_PREFIX: usize = 10_000;
const MAX_LOG_PREFIX: usize = 200;

pub(super) fn read(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    input: Value,
) -> Result<Value, OrbitError> {
    let workspace = required_string(&input, &["workspace"], "workspace")?;
    let scope = required_string(&input, &["scope"], "scope")?;
    // Drain selection has the readiness operation's own default and bounds;
    // other desktop projections retain their smaller page budget.
    let drain = scope == "drain";
    let limit = number(&input, "limit", if drain { 50 } else { 25 })?;
    if !drain && !(1..=MAX_PAGE).contains(&limit) {
        return Err(invalid("limit must be between 1 and 50"));
    }
    let mut result = match scope.as_str() {
        "tasks" => tasks(runtime, &input, limit)?,
        "task" => task(runtime, session, &input, limit)?,
        "runs" => runs(runtime, &input, limit)?,
        "drain" => super::drain::readiness(runtime, &input, limit)?,
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
    let (title, title_truncated) = bounded_redacted_text(&task.title, 512);
    let (crew, crew_truncated) = task
        .crew
        .as_deref()
        .map(|value| {
            let (value, truncated) = bounded_redacted_text(value, 128);
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
            "machine_id":bounded_redacted_text(&host.machine_id,512).0,
            "machine_name":host.machine_name.as_deref().map(|name| bounded_redacted_text(name,128).0),
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
    project_job_run(
        run,
        JobRunProjectionProfile::BoundedSummary,
        JobRunProjectionEvidence::default(),
    )
}

fn run(runtime: &OrbitRuntime, input: &Value, limit: usize) -> Result<Value, OrbitError> {
    let id = required_string(input, &["id"], "id")?;
    let offset = number(input, "log_offset", 0)?;
    prefix_bound(offset, MAX_LOG_PREFIX, "log_offset")?;
    let run = runtime.show_job_run_observed(&id)?;
    let mut value = run_summary(&run);
    let state = runtime.read_run_state(&id).ok().flatten();
    // Progress and the log page come from one bounded audit read; its total
    // is a store count, so a long history pages honestly.
    let page_limit = offset
        .saturating_add(limit)
        .min(MAX_LOG_PREFIX)
        .saturating_sub(offset);
    let detail =
        runtime.collect_bounded_run_detail(&id, offset, page_limit, BOUNDED_EXCERPT_MAX_BYTES)?;
    let steps = run_display_steps(&run, || detail.steps.clone());
    let steps_complete = steps.source == RunStepSource::Record || detail.steps_complete;
    // Reuse the lifecycle window; never replay an unbounded trail to populate
    // missing record steps.
    add_bounded_display_steps_to_json(&mut value, &steps, state.as_ref(), steps_complete);
    let progress = &detail.progress;
    value["agent_invocation"] = serde_json::to_value(crate::application::job::agent_invoke_result(
        &run,
        state.as_ref(),
        progress.provider_processes.last(),
    ))
    .map_err(super::super::json::serialize_error(
        "serialize agent invocation result",
    ))?;
    bound_bounded_agent_invocation_answer(
        &mut value["agent_invocation"],
        json!({
            "tool": "orbit.run",
            "arguments": {"action": "show", "workspace": input["workspace"], "id": id},
        }),
    )?;
    let execution_progress = run_execution_progress_to_json(progress);
    let total = detail.logs_total;
    let logs = detail
        .logs
        .iter()
        .map(|record| record.to_profile_json(RunCliInvocationProfile::BoundedExcerpt))
        .collect::<Vec<_>>();
    Ok(
        json!({"run": value, "usage":{"state":"unavailable"}, "execution_progress":execution_progress,
        "logs":{"state":detail.state,"pagination":pagination(offset,limit,logs.len(),total,MAX_LOG_PREFIX),
            "items":logs,"total":total,"available_through":MAX_LOG_PREFIX.min(total),"excerpt_max_bytes":BOUNDED_EXCERPT_MAX_BYTES},
        "audit_metadata":{"complete":detail.complete,"rows_read":detail.rows_read,"row_budget":detail.row_budget}}),
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
