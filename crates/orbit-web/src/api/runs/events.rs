//! Run audit events, invocation logs, and bounded log previews, each read from
//! the workspace that holds the run on this host ([`super::locate`]).

use crate::state::DashboardState;
use axum::extract::{Path, Query, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::app::run::{Command, exec};
use orbit_core::app::run::{RunEventsPage, RunEventsPageResult};
use serde_json::{Value, json};

use super::super::{
    HISTORY_DEFAULT_LIMIT, LimitQuery, RunEventsQuery, bad_request, blocking, bounded_limit,
    map_runtime_error, validate_id,
};
use super::locate::{RunLookupFailure, requested_workspace, with_run};

const RUN_EVENTS_DEFAULT_LIMIT: usize = 100;

/// `GET /api/runs/:id/events`, from the workspace that holds the run on this
/// host, located like run detail ([`super::locate`]).
pub(in crate::api) async fn list_run_events(
    State(state): State<DashboardState>,
    RawQuery(query): RawQuery,
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
    let requested = requested_workspace(query.as_deref());
    match blocking("run events", move || {
        let page = if q.tail {
            RunEventsPage::Tail { offset, limit }
        } else {
            RunEventsPage::Forward {
                kind: kind.as_deref(),
                offset,
                limit,
            }
        };
        let result = with_run(&state, requested.as_deref(), &run_id, |core, lookup| {
            exec(core, Command::dashboard_events(lookup, page))
        });
        Ok(result)
    })
    .await
    {
        Ok(Err(RunLookupFailure::Response(response))) => *response,
        Ok(Err(RunLookupFailure::Error(error))) => map_runtime_error(*error),
        Ok(Ok(RunEventsPageResult::Tail {
            events,
            total,
            offset,
        })) => Json(json!({"events": events, "total": total, "offset": offset})).into_response(),
        Ok(Ok(RunEventsPageResult::Forward(page))) => Json(Value::Array(page)).into_response(),
        Ok(Ok(RunEventsPageResult::ScanBudgetExceeded)) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({
                "error": "run-events audit rows exceed bounded scan budget; narrow the kind filter or reduce offset"
            })),
        )
            .into_response(),
        Err(response) => *response,
    }
}

/// `GET /api/runs/:id/logs`, located like run detail ([`super::locate`]). A
/// run recorded under a workspace no registered runtime serves has no CLI
/// invocation previews this host can resolve, so it answers an empty list.
pub(in crate::api) async fn list_run_logs(
    State(state): State<DashboardState>,
    RawQuery(query): RawQuery,
    Path(id): Path<String>,
    Query(q): Query<LimitQuery>,
) -> Response {
    let run_id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let limit = bounded_limit(q.limit, HISTORY_DEFAULT_LIMIT);
    let run_id = run_id.to_string();
    let requested = requested_workspace(query.as_deref());
    match blocking("run logs", move || {
        Ok(with_run(
            &state,
            requested.as_deref(),
            &run_id,
            |core, lookup| exec(core, Command::dashboard_logs(lookup, limit)),
        ))
    })
    .await
    {
        Ok(Ok(records)) => Json(Value::Array(records)).into_response(),
        Ok(Err(RunLookupFailure::Response(response))) => *response,
        Ok(Err(RunLookupFailure::Error(error))) => map_runtime_error(*error),
        Err(response) => *response,
    }
}
