//! `/diagnostics/{metrics,errors,friction}` aggregation endpoints.

use std::collections::{HashMap, HashSet};

use crate::state::Ws;
use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use chrono::DateTime;
use orbit_cmd::DiagnosticsCommands;
use orbit_common::security::redaction::redact_all;
use orbit_common::storage::blob_store::BlobStore;
use orbit_core::{InvocationQuery, InvocationRecord, OrbitRuntime, V2AuditEventFilter};
use serde_json::{Value, json};

use super::{
    DiagnosticsQuery, HISTORY_DEFAULT_LIMIT, bounded_limit, current_year_month_utc,
    map_runtime_error, month_bounds_utc, validate_year_month,
};
use crate::log_format::{
    Filters as LogFilters, format_message_html, format_source, read_recent_matching_events,
    resolve_log_path,
};

pub(super) async fn list_diagnostics_metrics(
    Ws(runtime): Ws,
    Query(q): Query<DiagnosticsQuery>,
) -> Response {
    let month = q.month.unwrap_or_else(current_year_month_utc);
    if let Err(e) = validate_year_month(&month) {
        return map_runtime_error(e);
    }
    let limit = bounded_limit(q.limit, HISTORY_DEFAULT_LIMIT);
    match super::blocking("diagnostics metrics", move || {
        let mut entries = runtime.read_metrics_entries_limited(&month, limit)?;
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.ts));
        entries.truncate(limit);
        if entries.is_empty() {
            diagnostics_metrics_from_invocations(&runtime, &month, limit).map(Value::Array)
        } else {
            serde_json::to_value(&entries).map_err(|e| orbit_core::OrbitError::Store(e.to_string()))
        }
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

fn diagnostics_metrics_from_invocations(
    runtime: &OrbitRuntime,
    month: &str,
    limit: usize,
) -> Result<Vec<Value>, orbit_core::OrbitError> {
    let (since, until) = month_bounds_utc(month)?;
    let records = runtime.invocation_records(InvocationQuery {
        since: Some(since),
        until: Some(until),
        limit,
        ..InvocationQuery::default()
    })?;

    Ok(diagnostics_metrics_values(records))
}

// Widened to pub(super) so tests under api/tests/ (per-module layout ORB-00224) can
// cover the metrics/friction row logic extracted from the old diagnostics_tests.rs sibling.
pub(super) fn diagnostics_metrics_values(records: Vec<InvocationRecord>) -> Vec<Value> {
    records
        .into_iter()
        .map(|record| {
            json!({
                "ts": record.ts.to_rfc3339(),
                "job_run": record.job_run_id,
                "step": record.activity_id,
                "task_id": record.task_ids.first().cloned(),
                "actor_identity": actor_label(&record.agent, record.model.as_deref()),
                "tool_invocations": record.tool_call_count,
                "token_usage": record.total_tokens,
                "step_duration_ms": record.duration_ms,
                "retry_count": 0,
            })
        })
        .collect()
}

fn actor_label(agent: &str, model: Option<&str>) -> String {
    match model.filter(|model| !model.is_empty()) {
        Some(model) if !agent.is_empty() => format!("{agent} / {model}"),
        Some(model) => model.to_string(),
        None => agent.to_string(),
    }
}

fn diagnostics_friction_from_v2_audit(
    runtime: &OrbitRuntime,
    month: &str,
    limit: usize,
) -> Result<Vec<Value>, orbit_core::OrbitError> {
    validate_year_month(month)?;
    if limit == 0 {
        return Ok(Vec::new());
    }

    let (since, until) = month_bounds_utc(month)?;
    let events = v2_audit_values(runtime, Some(since), Some(until), 50_000)?;
    let blob_store = audit_blob_store(runtime);
    let by_id = events_by_id(&events);
    let mut rows = Vec::new();
    for event in events {
        if let Some(row) = diagnostics_friction_row(&blob_store, &by_id, &event, month) {
            rows.push(row);
        }
    }

    rows.sort_by(|a, b| {
        let left = a.get("ts").and_then(Value::as_str).unwrap_or("");
        let right = b.get("ts").and_then(Value::as_str).unwrap_or("");
        right.cmp(left)
    });
    rows.truncate(limit);
    Ok(rows)
}

fn v2_audit_values(
    runtime: &OrbitRuntime,
    since: Option<chrono::DateTime<chrono::Utc>>,
    until: Option<chrono::DateTime<chrono::Utc>>,
    limit: usize,
) -> Result<Vec<Value>, orbit_core::OrbitError> {
    let rows = OrbitRuntime::list_v2_audit_events(
        runtime,
        V2AuditEventFilter {
            workspace_id: String::new(),
            since,
            until,
            source: Some("v2_envelope".to_string()),
            limit: Some(limit),
            ..Default::default()
        },
    )?;
    Ok(rows
        .into_iter()
        .rev()
        .filter_map(|row| serde_json::from_str::<Value>(&row.payload_json).ok())
        .collect())
}

fn events_by_id(events: &[Value]) -> HashMap<String, Value> {
    events
        .iter()
        .filter_map(|event| {
            event
                .get("event_id")
                .and_then(Value::as_str)
                .map(|event_id| (event_id.to_string(), event.clone()))
        })
        .collect()
}

fn audit_blob_store(runtime: &OrbitRuntime) -> BlobStore {
    BlobStore::new(
        runtime
            .data_root()
            .join("state")
            .join("audit")
            .join("blobs"),
    )
}

// Widened to pub(super) for api/tests/ access after test layout migration (ORB-00224).
pub(super) fn diagnostics_friction_row(
    blob_store: &BlobStore,
    events_by_id: &HashMap<String, Value>,
    event: &Value,
    month: &str,
) -> Option<Value> {
    let ts = event.get("ts").and_then(Value::as_str)?;
    if !ts.starts_with(month) {
        return None;
    }

    let body_kind = event.get("body_kind").and_then(Value::as_str).unwrap_or("");
    match body_kind {
        "cli_invocation_finished" => {
            let exit_code = event.get("exit_code").and_then(Value::as_i64);
            let timed_out = event
                .get("timed_out")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if exit_code == Some(0) && !timed_out {
                return None;
            }
            Some(json!({
                "ts": ts,
                "job_run": event.get("run_id").and_then(Value::as_str).unwrap_or(""),
                "step": enclosing_step_id_for_event(event, events_by_id).unwrap_or_default(),
                "task_id": null,
                "command": event.get("provider").and_then(Value::as_str).unwrap_or("cli"),
                "input": "",
                "exit_code": exit_code,
                "stderr": event
                    .get("stderr_blob_ref")
                    .and_then(Value::as_str)
                    .map(|blob_ref| read_blob_text_best_effort(blob_store, blob_ref))
                    .unwrap_or_default(),
                "actor_identity": event.get("agent_identity").cloned().unwrap_or(Value::Null),
            }))
        }
        "step_finished" => {
            let outcome = event
                .get("outcome")
                .and_then(Value::as_str)
                .unwrap_or("success");
            if matches!(outcome, "success" | "skipped") {
                return None;
            }
            let step = event.get("step_id").and_then(Value::as_str).unwrap_or("");
            Some(json!({
                "ts": ts,
                "job_run": event.get("run_id").and_then(Value::as_str).unwrap_or(""),
                "step": step,
                "task_id": null,
                "command": step,
                "input": "",
                "exit_code": null,
                "stderr": event
                    .get("error_message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("step finished with outcome '{outcome}'")),
                "actor_identity": event.get("agent_identity").cloned().unwrap_or(Value::Null),
            }))
        }
        "step_denied" | "tool_denied" | "fs_call_denied" => {
            let step = enclosing_step_id_for_event(event, events_by_id).unwrap_or_default();
            Some(json!({
                "ts": ts,
                "job_run": event.get("run_id").and_then(Value::as_str).unwrap_or(""),
                "step": step,
                "task_id": null,
                "command": event
                    .get("tool_name")
                    .or_else(|| event.get("op"))
                    .and_then(Value::as_str)
                    .unwrap_or(body_kind),
                "input": "",
                "exit_code": null,
                "stderr": event
                    .get("reason")
                    .or_else(|| event.get("matched_rule"))
                    .and_then(Value::as_str)
                    .unwrap_or(body_kind),
                "actor_identity": event.get("agent_identity").cloned().unwrap_or(Value::Null),
            }))
        }
        _ => None,
    }
}

fn enclosing_step_id_for_event(
    event: &Value,
    events_by_id: &HashMap<String, Value>,
) -> Option<String> {
    if let Some(step_id) = event.get("step_id").and_then(Value::as_str) {
        return Some(step_id.to_string());
    }

    let mut parent_id = event
        .get("parent_event_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut seen = HashSet::new();
    while let Some(id) = parent_id {
        if !seen.insert(id.clone()) {
            return None;
        }
        let parent = events_by_id.get(&id)?;
        if parent.get("body_kind").and_then(Value::as_str) == Some("step_started") {
            return parent
                .get("step_id")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        parent_id = parent
            .get("parent_event_id")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    None
}

fn read_blob_text_best_effort(blob_store: &BlobStore, blob_ref: &str) -> String {
    if blob_ref.len() < 2 || blob_ref.starts_with("error:") {
        return String::new();
    }
    blob_store
        .read(blob_ref)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

pub(super) async fn list_diagnostics_errors(
    Ws(runtime): Ws,
    Query(q): Query<DiagnosticsQuery>,
) -> Response {
    let limit = bounded_limit(q.limit, HISTORY_DEFAULT_LIMIT);
    // Reads up to 50k audit rows and a blob per agent invocation: blocking
    // pool, not the worker serving the request (see `blocking`).
    match super::blocking("diagnostics errors", move || {
        diagnostics_errors(&runtime, limit)
    })
    .await
    {
        Ok(rows) => Json(Value::Array(rows)).into_response(),
        Err(response) => *response,
    }
}

fn diagnostics_errors(
    runtime: &OrbitRuntime,
    limit: usize,
) -> Result<Vec<Value>, orbit_core::OrbitError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut rows = global_error_rows(limit)?;
    rows.extend(agent_stderr_error_rows(runtime, limit)?);
    rows.sort_by(|a, b| {
        let left = a.get("ts").and_then(Value::as_str).unwrap_or("");
        let right = b.get("ts").and_then(Value::as_str).unwrap_or("");
        right.cmp(left)
    });
    rows.truncate(limit);
    Ok(rows)
}

fn global_error_rows(limit: usize) -> Result<Vec<Value>, orbit_core::OrbitError> {
    let path = resolve_log_path(None)?;
    global_error_rows_from_path(&path, limit)
}

// Widened to pub(super) for api/tests/ access after test layout migration (ORB-00224).
pub(super) fn global_error_rows_from_path(
    path: &std::path::Path,
    limit: usize,
) -> Result<Vec<Value>, orbit_core::OrbitError> {
    let filters = LogFilters::from_query_parts(None, Some("error".to_string()), None)?;
    // Read the raw JSONL events so run/task/step/provider survive; the
    // rendered log-tail shape drops those fields.
    let events = read_recent_matching_events(path, &filters, limit)
        .map_err(|e| orbit_core::OrbitError::Io(format!("read log {}: {e}", path.display())))?;
    Ok(events
        .into_iter()
        .map(|event| process_error_row(&event))
        .collect())
}

fn process_error_row(event: &Value) -> Value {
    let ts = event.get("timestamp").and_then(Value::as_str).unwrap_or("");
    let target = event.get("target").and_then(Value::as_str).unwrap_or("-");
    let empty_fields = Value::Object(Default::default());
    let fields = event.get("fields").unwrap_or(&empty_fields);
    let job_run = optional_log_field(fields, &["job_run_id", "run_id"]);
    let affiliation = if job_run.is_some() {
        "run"
    } else {
        "unaffiliated"
    };

    json!({
        "ts": ts,
        "source": "process",
        "message": strip_htmlish(&format_message_html(target, fields)),
        "event_id": null,
        "job_run": job_run,
        "step": optional_log_field(fields, &["step_id", "step", "activity_id"]),
        "step_index": null,
        "task_id": optional_log_field(fields, &["task_id"]),
        "provider": optional_log_field(fields, &["provider"]),
        "blob_ref": null,
        "target": format_source(target, fields),
        "affiliation": affiliation,
    })
}

fn optional_log_field<'a>(fields: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        fields
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

fn agent_stderr_error_rows(
    runtime: &OrbitRuntime,
    limit: usize,
) -> Result<Vec<Value>, orbit_core::OrbitError> {
    let events = v2_audit_values(runtime, None, None, 50_000)?;
    let by_id = events_by_id(&events);
    let step_index_by_id = step_index_by_id(&events);
    let blob_store = audit_blob_store(runtime);
    let mut rows = Vec::new();
    // `events` is oldest-first so the step index above numbers steps in
    // execution order; the row scan walks newest-first because it stops at
    // `2 * limit` rows and the caller keeps only the newest `limit` of them.
    for event in events.iter().rev() {
        if event.get("body_kind").and_then(Value::as_str) != Some("cli_invocation_finished") {
            continue;
        }
        let Some(blob_ref) = event.get("stderr_blob_ref").and_then(Value::as_str) else {
            continue;
        };
        let stderr = read_blob_text_best_effort(&blob_store, blob_ref);
        let fallback_ts = event
            .get("ts")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let step = enclosing_step_id_for_event(event, &by_id);
        let step_index = step
            .as_ref()
            .and_then(|step| step_index_by_id.get(step).copied());
        for parsed in parse_structured_error_lines(&stderr, &fallback_ts) {
            rows.push(json!({
                "ts": parsed.ts,
                "source": "agent-stderr",
                "message": redact_all(&parsed.message),
                "job_run": event.get("run_id").and_then(Value::as_str),
                "step": step,
                "step_index": step_index,
                "task_id": event.get("task_id").and_then(Value::as_str),
                "provider": event.get("provider").and_then(Value::as_str),
                "blob_ref": blob_ref,
                "event_id": event.get("event_id").and_then(Value::as_str),
                "target": parsed.target,
            }));
            if rows.len() >= limit.saturating_mul(2) {
                return Ok(rows);
            }
        }
    }
    Ok(rows)
}

#[derive(Clone, Debug, PartialEq, Eq)]
// Widened to pub(super) to match the pub(super) on parse_structured_error_line (for api/tests/ layout migration ORB-00224).
pub(super) struct ParsedErrorLine {
    pub ts: String,
    pub target: String,
    pub message: String,
}

fn parse_structured_error_lines(stderr: &str, fallback_ts: &str) -> Vec<ParsedErrorLine> {
    stderr
        .lines()
        .filter_map(|line| parse_structured_error_line(line, fallback_ts))
        .collect()
}

// Widened to pub(super) for api/tests/ access after test layout migration (ORB-00224).
pub(super) fn parse_structured_error_line(
    line: &str,
    fallback_ts: &str,
) -> Option<ParsedErrorLine> {
    let trimmed = line.trim();
    let (ts, rest) = if let Some((head, tail)) = trimmed.split_once(" ERROR ") {
        if DateTime::parse_from_rfc3339(head).is_ok() {
            (head.to_string(), tail)
        } else {
            (fallback_ts.to_string(), trimmed.strip_prefix("ERROR ")?)
        }
    } else {
        (fallback_ts.to_string(), trimmed.strip_prefix("ERROR ")?)
    };
    let (target, message) = rest.rsplit_once(": ")?;
    let target = target.trim();
    let message = message.trim();
    if target.is_empty() || message.is_empty() {
        return None;
    }
    Some(ParsedErrorLine {
        ts,
        target: target.to_string(),
        message: message.to_string(),
    })
}

fn step_index_by_id(events: &[Value]) -> HashMap<String, u32> {
    let mut result = HashMap::new();
    for event in events {
        if event.get("body_kind").and_then(Value::as_str) != Some("step_started") {
            continue;
        }
        let Some(step_id) = event.get("step_id").and_then(Value::as_str) else {
            continue;
        };
        let index = result.len() as u32;
        result.entry(step_id.to_string()).or_insert(index);
    }
    result
}

fn strip_htmlish(raw: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
}

pub(super) async fn list_diagnostics_friction(
    Ws(runtime): Ws,
    Query(q): Query<DiagnosticsQuery>,
) -> Response {
    let month = q.month.unwrap_or_else(current_year_month_utc);
    if let Err(e) = validate_year_month(&month) {
        return map_runtime_error(e);
    }
    let limit = bounded_limit(q.limit, HISTORY_DEFAULT_LIMIT);
    match super::blocking("diagnostics friction", move || {
        let mut entries = runtime.read_friction_entries_limited(&month, limit)?;
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.ts));
        entries.truncate(limit);
        if entries.is_empty() {
            diagnostics_friction_from_v2_audit(&runtime, &month, limit).map(Value::Array)
        } else {
            serde_json::to_value(&entries).map_err(|e| orbit_core::OrbitError::Store(e.to_string()))
        }
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

pub(super) async fn diagnostics_implement_one(Ws(runtime): Ws) -> Response {
    let runtime_clone = runtime.clone();
    let aggregation =
        match tokio::task::spawn_blocking(move || compute_implement_one_by_actor(&runtime_clone))
            .await
        {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return map_runtime_error(e),
            Err(join_err) => {
                return map_runtime_error(orbit_core::OrbitError::Execution(format!(
                    "implement_one aggregation panicked: {join_err}"
                )));
            }
        };

    Json(json!({
        "implement_one_by_actor": aggregation.by_actor,
        "implement_one_by_complexity": aggregation.by_complexity,
    }))
    .into_response()
}

fn compute_implement_one_by_actor(
    runtime: &OrbitRuntime,
) -> Result<ImplementOneAggregation, orbit_core::OrbitError> {
    let since = chrono::Utc::now() - chrono::Duration::days(30);

    let records = runtime.invocation_records(orbit_core::InvocationQuery {
        since: Some(since),
        until: None,
        activity_id: Some("implement_one".to_string()),
        limit: 100_000,
        ..Default::default()
    })?;
    let complexity_by_task = runtime.task_complexity_by_id()?;

    let mut durations_by_actor: HashMap<String, Vec<i64>> = HashMap::new();
    let mut durations_by_complexity_actor: HashMap<String, HashMap<String, Vec<i64>>> =
        HashMap::new();
    for record in records {
        let actor = actor_label(&record.agent, record.model.as_deref());
        let complexity = record
            .task_ids
            .first()
            .and_then(|task_id| complexity_by_task.get(task_id).cloned())
            .unwrap_or_else(|| orbit_types::task::UNSET_BUCKET.to_string());
        durations_by_actor
            .entry(actor.clone())
            .or_default()
            .push(record.duration_ms as i64);
        durations_by_complexity_actor
            .entry(complexity)
            .or_default()
            .entry(actor)
            .or_default()
            .push(record.duration_ms as i64);
    }

    let by_actor = actor_duration_rows(durations_by_actor);
    let mut by_complexity: Vec<Value> = durations_by_complexity_actor
        .into_iter()
        .map(|(complexity, actors)| {
            let actor_rows = actor_duration_rows(actors);
            let n: usize = actor_rows
                .iter()
                .map(|row| row["n"].as_u64().unwrap_or(0) as usize)
                .sum();
            json!({
                "complexity": complexity,
                "n": n,
                "actors": actor_rows,
            })
        })
        .collect();
    by_complexity.sort_by(|left, right| {
        orbit_types::task::complexity_bucket_ord(
            left["complexity"]
                .as_str()
                .unwrap_or(orbit_types::task::UNSET_BUCKET),
        )
        .cmp(&orbit_types::task::complexity_bucket_ord(
            right["complexity"]
                .as_str()
                .unwrap_or(orbit_types::task::UNSET_BUCKET),
        ))
    });

    Ok(ImplementOneAggregation {
        by_actor,
        by_complexity,
    })
}

struct ImplementOneAggregation {
    by_actor: Vec<Value>,
    by_complexity: Vec<Value>,
}

fn actor_duration_rows(durations_by_actor: HashMap<String, Vec<i64>>) -> Vec<Value> {
    let mut actor_vec: Vec<_> = durations_by_actor
        .into_iter()
        .map(|(actor, mut durations)| {
            durations.sort_unstable();
            let n = durations.len();
            let avg = if n > 0 {
                durations.iter().sum::<i64>() as f64 / n as f64
            } else {
                0.0
            };
            let p50_idx = ((n as f64 * 0.50).ceil() as usize).min(n).saturating_sub(1);
            let p50 = if n > 0 { durations[p50_idx] } else { 0 };
            let p95_idx = ((n as f64 * 0.95).ceil() as usize).min(n).saturating_sub(1);
            let p95 = if n > 0 { durations[p95_idx] } else { 0 };
            json!({
                "actor": actor,
                "n": n,
                "avg": avg,
                "p50": p50,
                "p95": p95,
            })
        })
        .collect();

    actor_vec.sort_by(|a, b| {
        b["avg"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["avg"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    actor_vec
}
