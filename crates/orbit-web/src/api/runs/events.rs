//! Run audit events, invocation logs, and bounded log previews.

use crate::state::Ws;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::runtime::audit::run_events::{RunEventsPage, RunEventsPageResult};
use orbit_core::runtime::audit::run_logs::RunCliInvocationProfile;
use serde_json::{Value, json};

use super::super::{
    HISTORY_DEFAULT_LIMIT, LimitQuery, RunEventsQuery, bad_request, blocking, bounded_limit,
    validate_id,
};

const RUN_EVENTS_DEFAULT_LIMIT: usize = 100;

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
        let run = runtime.show_job_run(&run_id)?;
        let page = if q.tail {
            RunEventsPage::Tail { offset, limit }
        } else {
            RunEventsPage::Forward {
                kind: kind.as_deref(),
                offset,
                limit,
            }
        };
        runtime.run_events_page(&run, page)
    })
    .await
    {
        Ok(RunEventsPageResult::Tail {
            events,
            total,
            offset,
        }) => Json(json!({"events": events, "total": total, "offset": offset})).into_response(),
        Ok(RunEventsPageResult::Forward(page)) => Json(Value::Array(page)).into_response(),
        Ok(RunEventsPageResult::ScanBudgetExceeded) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({
                "error": "run-events audit rows exceed bounded scan budget; narrow the kind filter or reduce offset"
            })),
        )
            .into_response(),
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
            RunCliInvocationProfile::HttpPreview.blob_read_max_bytes(),
        )
    })
    .await
    {
        Ok(records) => Json(Value::Array(
            records
                .iter()
                .map(|record| record.to_profile_json(RunCliInvocationProfile::HttpPreview))
                .collect(),
        ))
        .into_response(),
        Err(response) => *response,
    }
}
