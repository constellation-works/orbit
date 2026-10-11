//! Run audit events, invocation logs, and bounded log previews.

use crate::state::Ws;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::security::redaction::redact_all;
use orbit_core::V2AuditEventFilter;
use orbit_core::runtime::audit::run::RunCliInvocationRecord;
use serde_json::{Value, json};

use super::super::{
    HISTORY_DEFAULT_LIMIT, LimitQuery, RunEventsQuery, bad_request, blocking, bounded_limit,
    validate_id,
};

const RUN_EVENTS_DEFAULT_LIMIT: usize = 100;
/// Hard cap on rows scanned from a single run's persisted v2 audit events.
pub(in crate::api) const RUN_EVENTS_MAX_SCAN_LINES: usize = 50_000;
/// Capacity hint for the returned events page. A fixed constant rather than
/// the caller-supplied `limit` so the allocation size never derives from
/// request input, regardless of how large a `limit` a caller requests.
const RUN_EVENTS_PAGE_CAPACITY_HINT: usize = 64;
/// Maximum bytes included in stdout/stderr previews returned by run-log APIs.
pub(in crate::api) const RUN_LOG_PREVIEW_MAX_BYTES: usize = 8192;
/// Maximum lines included in stdout/stderr previews returned by run-log APIs.
const RUN_LOG_PREVIEW_MAX_LINES: usize = 120;

pub(in crate::api) async fn list_run_events(
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Query(q): Query<RunEventsQuery>,
) -> Response {
    let run_id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let limit = bounded_limit(q.limit, RUN_EVENTS_DEFAULT_LIMIT);
    let offset = q.offset.unwrap_or(0);
    let kind = q
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if q.tail && kind.is_some() {
        return bad_request("tail pages do not support a kind filter".to_string());
    }

    let run_id = run_id.to_string();
    match blocking("run events", move || {
        runtime.show_job_run(&run_id)?;
        if q.tail {
            let filter = V2AuditEventFilter {
                workspace_id: runtime.workspace_id()?,
                run_id: Some(run_id),
                source: Some("v2_envelope".to_string()),
                limit: Some(limit),
                offset: Some(offset),
                ..Default::default()
            };
            let store = runtime.v2_audit_store()?;
            // One read snapshot: a row committed after the count cannot move
            // this page relative to the total that describes it.
            let page = store.list_v2_audit_event_tail(&filter)?;
            let total = usize::try_from(page.total).unwrap_or(0);
            let events: Vec<Value> = page
                .events
                .into_iter()
                .filter_map(|row| serde_json::from_str(&row.payload_json).ok())
                .collect();
            return Ok((json!({"events": events, "total": total, "offset": offset}), false));
        }
        // Pages read forward from the run's first event. Without a kind
        // filter the page is a plain SQL window; `body_kind` lives inside the
        // payload, so a filtered page scans under a row budget instead.
        let (sql_offset, sql_limit, skip) = match kind {
            None => (offset, limit, 0),
            Some(_) => (0, RUN_EVENTS_MAX_SCAN_LINES + 1, offset),
        };
        let rows = runtime.list_v2_audit_events(V2AuditEventFilter {
            workspace_id: String::new(),
            run_id: Some(run_id),
            source: Some("v2_envelope".to_string()),
            limit: Some(sql_limit),
            offset: Some(sql_offset),
            oldest_first: true,
            ..Default::default()
        })?;
        let budget_exceeded = kind.is_some() && rows.len() > RUN_EVENTS_MAX_SCAN_LINES;
        let mut page: Vec<Value> = Vec::with_capacity(RUN_EVENTS_PAGE_CAPACITY_HINT);
        let mut matched: usize = 0;

        for row in rows.into_iter().take(RUN_EVENTS_MAX_SCAN_LINES) {
            let value: Value = match serde_json::from_str(&row.payload_json) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(ref needle) = kind {
                let body_kind = value.get("body_kind").and_then(Value::as_str).unwrap_or("");
                if body_kind != needle {
                    continue;
                }
            }
            if matched < skip {
                matched = matched.saturating_add(1);
                continue;
            }
            page.push(value);
            matched = matched.saturating_add(1);
            if page.len() >= limit {
                break;
            }
        }

        Ok((Value::Array(page), budget_exceeded))
    })
    .await
    {
        Ok((page, budget_exceeded)) if budget_exceeded && page.as_array().is_some_and(|p| p.len() < limit) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({
                "error": "run-events audit rows exceed bounded scan budget; narrow the kind filter or reduce offset"
            })),
        )
            .into_response(),
        Ok((page, _)) => Json(page).into_response(),
        Err(response) => *response,
    }
}

pub(in crate::api) async fn list_run_logs(
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Query(q): Query<LimitQuery>,
) -> Response {
    let run_id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let limit = bounded_limit(q.limit, HISTORY_DEFAULT_LIMIT);
    let run_id = run_id.to_string();
    match blocking("run logs", move || {
        runtime.show_job_run(&run_id)?;
        runtime.collect_run_cli_invocations_bounded(
            &run_id,
            Some(limit),
            Some(RUN_LOG_PREVIEW_MAX_BYTES),
        )
    })
    .await
    {
        Ok(records) => Json(Value::Array(
            records
                .into_iter()
                .map(run_cli_invocation_to_json)
                .collect(),
        ))
        .into_response(),
        Err(response) => *response,
    }
}

fn run_cli_invocation_to_json(record: RunCliInvocationRecord) -> Value {
    let stdout_preview = bounded_preview(&record.stdout);
    let stderr_preview = bounded_preview(&record.stderr);
    let stdout_truncated = stdout_preview.truncated || record.stdout_blob_truncated;
    let stderr_truncated = stderr_preview.truncated || record.stderr_blob_truncated;
    json!({
        "run_id": record.run_id,
        "event_id": record.event_id,
        "ts": record.ts.map(|ts| ts.to_rfc3339()),
        "step_id": record.step_id,
        "step_index": record.step_index,
        "provider": record.provider,
        "stdout_blob_ref": record.stdout_blob_ref,
        "stderr_blob_ref": record.stderr_blob_ref,
        "stdout_preview": stdout_preview.text,
        "stderr_preview": stderr_preview.text,
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated,
        "exit_code": record.exit_code,
        "timed_out": record.timed_out,
        "duration_ms": record.duration_ms,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Preview {
    text: String,
    truncated: bool,
}

fn bounded_preview(raw: &str) -> Preview {
    let mut out = String::new();
    let mut truncated = false;
    for (index, line) in raw.lines().enumerate() {
        if index >= RUN_LOG_PREVIEW_MAX_LINES {
            truncated = true;
            break;
        }
        let needed = line.len() + usize::from(!out.is_empty());
        if out.len().saturating_add(needed) > RUN_LOG_PREVIEW_MAX_BYTES {
            if out.is_empty() {
                for ch in line.chars() {
                    if out.len().saturating_add(ch.len_utf8()) > RUN_LOG_PREVIEW_MAX_BYTES {
                        break;
                    }
                    out.push(ch);
                }
            }
            truncated = true;
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    if raw.ends_with('\n') && !out.is_empty() && out.len() < RUN_LOG_PREVIEW_MAX_BYTES {
        out.push('\n');
    }
    Preview {
        text: redact_all(&out),
        truncated,
    }
}
