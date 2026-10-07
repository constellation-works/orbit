//! `/diagnostics/{metrics,errors,friction}` aggregation endpoints.

use std::collections::{HashMap, HashSet};

use std::sync::Arc;

use crate::parse::parse_since;
use crate::runtime_memo::{DIAGNOSTICS_ERRORS_TTL, DIAGNOSTICS_FRICTION_TTL};
use crate::state::{DashboardState, Ws};
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_cmd::DiagnosticsCommands;
use orbit_common::security::redaction::redact_all;
use orbit_common::storage::blob_store::BlobStore;
use orbit_core::{InvocationQuery, InvocationRecord, OrbitRuntime, V2AuditEventFilter};
use serde_json::{Value, json};

use super::runs::{RUN_LOG_PREVIEW_MAX_BYTES, bounded_preview};
use super::{
    DiagnosticsQuery, HISTORY_DEFAULT_LIMIT, bounded_limit, current_year_month_utc,
    map_runtime_error, month_bounds_utc, validate_year_month,
};
use crate::log_format::{
    Filters as LogFilters, format_message_html, read_recent_matching_events, resolve_log_path,
};

pub(super) async fn list_diagnostics_metrics(
    Ws(runtime): Ws,
    Query(q): Query<DiagnosticsQuery>,
) -> Response {
    let (since, until) = match diagnostics_bounds(&q, true) {
        Ok(bounds) => bounds,
        Err(e) => return map_runtime_error(e),
    };
    let limit = bounded_limit(q.limit, HISTORY_DEFAULT_LIMIT);
    match super::blocking("diagnostics metrics", move || {
        let mut entries = Vec::new();
        for month in runtime.list_metrics_months()? {
            let (month_start, month_end) = month_bounds_utc(&month)?;
            if since.is_some_and(|since| month_end <= since) || month_start >= until {
                continue;
            }
            entries.extend(
                runtime
                    .read_metrics_entries(&month)?
                    .into_iter()
                    .filter(|entry| {
                        since.is_none_or(|since| entry.ts >= since) && entry.ts < until
                    }),
            );
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.ts));
        entries.truncate(limit);
        if entries.is_empty() {
            let records = runtime.invocation_records(InvocationQuery {
                since,
                until: Some(until),
                limit,
                ..InvocationQuery::default()
            })?;
            Ok(Value::Array(diagnostics_metrics_values(records)))
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

// Explicit since wins over the legacy metrics month. Errors without a range
// retain the unbounded recent feed; the dashboard always supplies its window.
fn diagnostics_bounds(
    q: &DiagnosticsQuery,
    default_month: bool,
) -> Result<(Option<DateTime<Utc>>, DateTime<Utc>), orbit_core::OrbitError> {
    if let Some(raw) = q.since.as_deref() {
        let since = if raw == "all" {
            None
        } else {
            Some(parse_since(raw)?)
        };
        return Ok((since, Utc::now()));
    }
    if q.month.is_some() || default_month {
        let month = q.month.clone().unwrap_or_else(current_year_month_utc);
        let (since, until) = month_bounds_utc(&month)?;
        return Ok((Some(since), until));
    }
    Ok((None, Utc::now()))
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
    // `events` is oldest-first; walk newest-first and stop at `limit` so only
    // the rows returned pay for their stderr blob read.
    let mut rows = Vec::new();
    for event in events.iter().rev() {
        if let Some(row) = diagnostics_friction_row(&blob_store, &by_id, event, month) {
            rows.push(row);
            if rows.len() == limit {
                break;
            }
        }
    }
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

fn events_by_id(events: &[Value]) -> HashMap<&str, &Value> {
    events
        .iter()
        .filter_map(|event| {
            event
                .get("event_id")
                .and_then(Value::as_str)
                .map(|event_id| (event_id, event))
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
pub(super) fn diagnostics_friction_row<'a>(
    blob_store: &BlobStore,
    events_by_id: &HashMap<&'a str, &'a Value>,
    event: &'a Value,
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
                    .map(|blob_ref| read_blob_preview_best_effort(blob_store, blob_ref))
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

fn enclosing_step_id_for_event<'a>(
    event: &'a Value,
    events_by_id: &HashMap<&'a str, &'a Value>,
) -> Option<String> {
    if let Some(step_id) = event.get("step_id").and_then(Value::as_str) {
        return Some(step_id.to_string());
    }

    let mut parent_id = event.get("parent_event_id").and_then(Value::as_str);
    let mut seen = HashSet::new();
    while let Some(id) = parent_id {
        if !seen.insert(id) {
            return None;
        }
        let parent = events_by_id.get(id)?;
        if parent.get("body_kind").and_then(Value::as_str) == Some("step_started") {
            return parent
                .get("step_id")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        parent_id = parent.get("parent_event_id").and_then(Value::as_str);
    }
    None
}

fn read_blob_preview_best_effort(blob_store: &BlobStore, blob_ref: &str) -> String {
    // One extra byte distinguishes an exact-cap blob from a truncated one
    // without loading the rest of the file.
    let Ok(bytes) = blob_store.read_prefix(blob_ref, RUN_LOG_PREVIEW_MAX_BYTES + 1) else {
        return String::new();
    };
    let mut preview = bounded_preview(&String::from_utf8_lossy(&bytes));
    if preview.truncated || bytes.len() > RUN_LOG_PREVIEW_MAX_BYTES {
        preview.text.push_str("\n[truncated]");
    }
    preview.text
}

fn read_blob_text_best_effort(blob_store: &BlobStore, blob_ref: &str) -> String {
    blob_store
        .read(blob_ref)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

pub(super) async fn list_diagnostics_errors(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(q): Query<DiagnosticsQuery>,
) -> Response {
    let (since, until) = match diagnostics_bounds(&q, false) {
        Ok(bounds) => bounds,
        Err(e) => return map_runtime_error(e),
    };
    let key = (
        q.since.clone(),
        q.month.clone(),
        bounded_limit(q.limit, HISTORY_DEFAULT_LIMIT),
    );
    let limit = bounded_limit(q.limit, HISTORY_DEFAULT_LIMIT);
    // Reads up to 50k audit rows and a blob per agent invocation, and every
    // Errors tab polls it: memoized so overlapping polls share one scan, and
    // computed on the blocking pool, not the worker serving the request.
    let compute_runtime = Arc::clone(&runtime);
    match state
        .diagnostics_errors_memo()
        .get_or_compute(&runtime, key, DIAGNOSTICS_ERRORS_TTL, move || {
            diagnostics_errors(&compute_runtime, limit, since, until).map(Value::Array)
        })
        .await
    {
        Ok(rows) => Json((*rows).clone()).into_response(),
        Err(error) => map_runtime_error(error),
    }
}

fn diagnostics_errors(
    runtime: &OrbitRuntime,
    limit: usize,
    since: Option<DateTime<Utc>>,
    until: DateTime<Utc>,
) -> Result<Vec<Value>, orbit_core::OrbitError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let events = v2_audit_values(runtime, since, Some(until), 50_000)?;
    let mut rows = global_error_rows(limit, since, until)?;
    for row in &mut rows {
        if row["target"] != "orbit.job.step_finished"
            || !row["job_run"].is_string()
            || !row["step"].is_string()
        {
            continue;
        }
        // Run + step alone is ambiguous across retries. Pick the finish event
        // nearest this process record, rather than another attempt's failure.
        let ts = row["ts"]
            .as_str()
            .and_then(|s| s.parse::<DateTime<Utc>>().ok());
        let nearest = events
            .iter()
            .filter(|event| {
                event["body_kind"] == "step_finished"
                    && event["run_id"] == row["job_run"]
                    && event["step_id"] == row["step"]
                    && event["error_message"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty())
            })
            .min_by_key(|event| {
                event["ts"]
                    .as_str()
                    .and_then(|s| s.parse::<DateTime<Utc>>().ok())
                    .zip(ts)
                    .map_or(i64::MAX, |(event_ts, ts)| {
                        (event_ts - ts).num_milliseconds().saturating_abs()
                    })
            });
        if let Some(event) = nearest {
            row["message"] = json!(redact_all(
                event["error_message"].as_str().unwrap_or_default()
            ));
            row["event_id"] = event["event_id"].clone();
        }
    }
    rows.extend(agent_stderr_error_rows(
        runtime, &events, limit, since, until,
    )?);
    rows.retain(|row| {
        row["ts"]
            .as_str()
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
            .is_some_and(|ts| since.is_none_or(|since| ts >= since) && ts < until)
    });
    rows.sort_by(|a, b| {
        let left = a.get("ts").and_then(Value::as_str).unwrap_or("");
        let right = b.get("ts").and_then(Value::as_str).unwrap_or("");
        right.cmp(left)
    });
    let mut seen = HashSet::new();
    rows.retain(|row| {
        row["event_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .is_none_or(|id| seen.insert(id.to_string()))
    });
    rows.truncate(limit);
    Ok(rows)
}

fn global_error_rows(
    limit: usize,
    since: Option<DateTime<Utc>>,
    until: DateTime<Utc>,
) -> Result<Vec<Value>, orbit_core::OrbitError> {
    let path = resolve_log_path(None)?;
    let filters = LogFilters::new(None, Some(crate::log_format::LevelFilter::Error), since);
    let events = read_recent_matching_events(&path, &filters, limit.saturating_mul(2))
        .map_err(|e| orbit_core::OrbitError::Io(format!("read log {}: {e}", path.display())))?;
    Ok(events
        .iter()
        .filter(|event| {
            event["timestamp"]
                .as_str()
                .and_then(|s| s.parse::<DateTime<Utc>>().ok())
                .is_some_and(|ts| ts < until)
        })
        .map(process_error_row)
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
        "message": optional_log_field(fields, &["error_message"]).map(redact_all)
            .unwrap_or_else(|| strip_htmlish(&format_message_html(target, fields))),
        "event_id": optional_log_field(fields, &["event_id"]),
        "job_run": job_run,
        "step": optional_log_field(fields, &["step_id", "step", "activity_id"]),
        "step_index": null,
        "task_id": optional_log_field(fields, &["task_id"]),
        "provider": optional_log_field(fields, &["provider"]),
        "blob_ref": null,
        "target": target,
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

/// Most stderr blobs one `/api/diagnostics/errors` computation reads from
/// disk. Each `cli_invocation_finished` event costs one blob read, so a long
/// history of invocations with no structured error lines would otherwise read
/// thousands of files per poll. Newest invocations are read first, so the cap
/// only ever drops the oldest.
pub(super) const MAX_STDERR_BLOBS_PER_REQUEST: usize = 256;

fn agent_stderr_error_rows(
    runtime: &OrbitRuntime,
    events: &[Value],
    limit: usize,
    since: Option<DateTime<Utc>>,
    until: DateTime<Utc>,
) -> Result<Vec<Value>, orbit_core::OrbitError> {
    let by_id = events_by_id(events);
    let step_index_by_id = step_index_by_id(events);
    let blob_store = audit_blob_store(runtime);
    let mut rows = Vec::new();
    let mut blobs_read = 0usize;
    let mut seen = HashSet::new();
    // `events` is oldest-first so the step index above numbers steps in
    // execution order; the row scan walks newest-first because it stops at
    // `2 * limit` rows and the caller keeps only the newest `limit` of them.
    for event in events.iter().rev() {
        if event.get("body_kind").and_then(Value::as_str) != Some("cli_invocation_finished") {
            continue;
        }
        if let Some(id) = event["event_id"].as_str()
            && !seen.insert(id)
        {
            continue;
        }
        let Some(blob_ref) = event.get("stderr_blob_ref").and_then(Value::as_str) else {
            continue;
        };
        if blobs_read >= MAX_STDERR_BLOBS_PER_REQUEST {
            break;
        }
        blobs_read += 1;
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
        let parsed: Vec<_> = parse_structured_error_lines(&stderr, &fallback_ts)
            .into_iter()
            .filter(|line| {
                line.ts
                    .parse::<DateTime<Utc>>()
                    .ok()
                    .is_some_and(|ts| since.is_none_or(|since| ts >= since) && ts < until)
            })
            .collect();
        if let Some(first) = parsed.first() {
            let mut messages = Vec::new();
            let mut targets = Vec::new();
            for line in &parsed {
                if !messages.contains(&line.message) {
                    messages.push(line.message.clone());
                }
                if !targets.contains(&line.target) {
                    targets.push(line.target.clone());
                }
            }
            rows.push(json!({
                "ts": first.ts,
                "source": "agent-stderr",
                "message": redact_all(&messages.join("\n")),
                "job_run": event.get("run_id").and_then(Value::as_str),
                "step": step,
                "step_index": step_index,
                "task_id": event.get("task_id").and_then(Value::as_str),
                "provider": event.get("provider").and_then(Value::as_str),
                "blob_ref": blob_ref,
                "event_id": event.get("event_id").and_then(Value::as_str),
                "target": targets.join(", "),
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
    let (target, message) = rest.split_once(": ")?;
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
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(q): Query<DiagnosticsQuery>,
) -> Response {
    let month = q.month.unwrap_or_else(current_year_month_utc);
    if let Err(e) = validate_year_month(&month) {
        return map_runtime_error(e);
    }
    let limit = bounded_limit(q.limit, HISTORY_DEFAULT_LIMIT);
    let compute_runtime = Arc::clone(&runtime);
    match state
        .diagnostics_friction_memo()
        .get_or_compute(
            &runtime,
            (month.clone(), limit),
            DIAGNOSTICS_FRICTION_TTL,
            move || {
                let mut entries = compute_runtime.read_friction_entries_limited(&month, limit)?;
                entries.sort_by_key(|entry| std::cmp::Reverse(entry.ts));
                entries.truncate(limit);
                if entries.is_empty() {
                    diagnostics_friction_from_v2_audit(&compute_runtime, &month, limit)
                        .map(Value::Array)
                } else {
                    serde_json::to_value(&entries)
                        .map_err(|e| orbit_core::OrbitError::Store(e.to_string()))
                }
            },
        )
        .await
    {
        Ok(rows) => Json((*rows).clone()).into_response(),
        Err(error) => map_runtime_error(error),
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
