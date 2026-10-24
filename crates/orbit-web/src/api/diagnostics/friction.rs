//! Friction rows and enclosing-step attribution.

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_cmd::DiagnosticsCommands;
use orbit_common::storage::blob_store::BlobStore;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::audit::run_logs::HTTP_PREVIEW_MAX_BYTES;
use orbit_types::record::FrictionEntry;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::super::{
    DiagnosticsQuery, Excerpt, HISTORY_DEFAULT_LIMIT, bounded_limit, current_year_month_utc,
    map_runtime_error, message_excerpt, month_bounds_utc, validate_year_month,
};
use super::audit::{audit_blob_store, events_by_id, v2_audit_values};
use crate::runtime_memo::DASHBOARD_POLL_MEMO_TTL;
use crate::state::{DashboardState, Ws};

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
            let stderr = event
                .get("stderr_blob_ref")
                .and_then(Value::as_str)
                .map(|blob_ref| read_blob_excerpt_best_effort(blob_store, blob_ref))
                .unwrap_or_else(|| message_excerpt(""));
            Some(json!({
                "ts": ts,
                "job_run": event.get("run_id").and_then(Value::as_str).unwrap_or(""),
                "step": enclosing_step_id_for_event(event, events_by_id).unwrap_or_default(),
                "task_id": null,
                "command": event.get("provider").and_then(Value::as_str).unwrap_or("cli"),
                "input": "",
                "exit_code": exit_code,
                "stderr": stderr.text,
                "truncated": stderr.truncated,
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
            let stderr = match event.get("error_message").and_then(Value::as_str) {
                Some(message) => message_excerpt(message),
                None => message_excerpt(&format!("step finished with outcome '{outcome}'")),
            };
            Some(json!({
                "ts": ts,
                "job_run": event.get("run_id").and_then(Value::as_str).unwrap_or(""),
                "step": step,
                "task_id": null,
                "command": step,
                "input": "",
                "exit_code": null,
                "stderr": stderr.text,
                "truncated": stderr.truncated,
                "actor_identity": event.get("agent_identity").cloned().unwrap_or(Value::Null),
            }))
        }
        "step_denied" | "tool_denied" | "fs_call_denied" => {
            let step = enclosing_step_id_for_event(event, events_by_id).unwrap_or_default();
            let stderr = message_excerpt(
                event
                    .get("reason")
                    .or_else(|| event.get("matched_rule"))
                    .and_then(Value::as_str)
                    .unwrap_or(body_kind),
            );
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
                "stderr": stderr.text,
                "truncated": stderr.truncated,
                "actor_identity": event.get("agent_identity").cloned().unwrap_or(Value::Null),
            }))
        }
        _ => None,
    }
}

pub(super) fn enclosing_step_id_for_event<'a, E: EnvelopeLinks>(
    event: &'a E,
    events_by_id: &HashMap<&'a str, &'a E>,
) -> Option<String> {
    if let Some(step_id) = event.step_id() {
        return Some(step_id.to_string());
    }

    let mut parent_id = event.parent_event_id();
    let mut seen = HashSet::new();
    while let Some(id) = parent_id {
        if !seen.insert(id) {
            return None;
        }
        let parent = events_by_id.get(id)?;
        if parent.body_kind() == Some("step_started") {
            return parent.step_id().map(str::to_string);
        }
        parent_id = parent.parent_event_id();
    }
    None
}

/// The envelope links a step attribution walk reads, from either a full
/// event payload or the Errors feed's compact stderr scan event.
pub(super) trait EnvelopeLinks {
    fn step_id(&self) -> Option<&str>;
    fn parent_event_id(&self) -> Option<&str>;
    fn body_kind(&self) -> Option<&str>;
}

impl EnvelopeLinks for Value {
    fn step_id(&self) -> Option<&str> {
        self.get("step_id").and_then(Value::as_str)
    }

    fn parent_event_id(&self) -> Option<&str> {
        self.get("parent_event_id").and_then(Value::as_str)
    }

    fn body_kind(&self) -> Option<&str> {
        self.get("body_kind").and_then(Value::as_str)
    }
}

fn read_blob_excerpt_best_effort(blob_store: &BlobStore, blob_ref: &str) -> Excerpt {
    // Reads the run-log preview prefix, which is wider than the excerpt, so
    // redaction sees past the cut; one extra byte tells an exact-size blob
    // from a longer one without loading the rest of the file.
    let Ok(bytes) = blob_store.read_prefix(blob_ref, HTTP_PREVIEW_MAX_BYTES + 1) else {
        return message_excerpt("");
    };
    let mut excerpt = message_excerpt(&String::from_utf8_lossy(&bytes));
    excerpt.truncated |= bytes.len() > HTTP_PREVIEW_MAX_BYTES;
    excerpt
}

/// A legacy friction record with its `stderr` bounded like the audit-derived
/// rows. The record's other fields keep their persisted shape.
fn friction_entry_to_json(entry: &FrictionEntry) -> Result<Value, orbit_core::OrbitError> {
    let stderr = message_excerpt(&entry.stderr);
    let mut row =
        serde_json::to_value(entry).map_err(|e| orbit_core::OrbitError::Store(e.to_string()))?;
    row["stderr"] = json!(stderr.text);
    row["truncated"] = json!(stderr.truncated);
    Ok(row)
}

pub(in crate::api) async fn list_diagnostics_friction(
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
            DASHBOARD_POLL_MEMO_TTL,
            move || {
                let mut entries = compute_runtime.read_friction_entries_limited(&month, limit)?;
                entries.sort_by_key(|entry| std::cmp::Reverse(entry.ts));
                entries.truncate(limit);
                if entries.is_empty() {
                    diagnostics_friction_from_v2_audit(&compute_runtime, &month, limit)
                        .map(Value::Array)
                } else {
                    entries
                        .iter()
                        .map(friction_entry_to_json)
                        .collect::<Result<Vec<_>, _>>()
                        .map(Value::Array)
                }
            },
        )
        .await
    {
        Ok(served) => (served.age_header(), Json((*served.body).clone())).into_response(),
        Err(error) => map_runtime_error(error),
    }
}
