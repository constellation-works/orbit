//! `GET /api/cost`: managed-execution cost and token usage for the dashboard
//! Cost page, and its deprecated `GET /api/scoreboard` alias.

use crate::runtime_memo::{DASHBOARD_POLL_MEMO_TTL, Served};
use crate::state::{DashboardState, Ws};
use axum::extract::{Query, State};
use axum::http::{HeaderName, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::OrbitRuntime;
use orbit_core::metrics::cost::MetricsWindow;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

use super::map_runtime_error;

/// [ORB-15922] `/api/scoreboard` served the retired agent-family scoreboard.
/// Until this date it answers with the cost payload under deprecation
/// headers; then the route is deleted.
const SCOREBOARD_SUNSET: &str = "Thu, 12 Nov 2026 00:00:00 GMT";
const SCOREBOARD_DEPRECATION: &str = "GET /api/scoreboard is deprecated: the agent-family \
     scoreboard was retired, and this route serves only the managed-execution cost subset \
     until 2026-11-12. Use GET /api/cost.";

/// Query-string shape for `GET /api/cost` and `GET /api/scoreboard`.
///
/// `?window=<1h|24h|7d|30d|all>` scopes the accounting. A missing parameter
/// is the lifetime window; an unknown value is HTTP 400.
#[derive(Debug, Default, Deserialize)]
pub(super) struct CostQuery {
    #[serde(default)]
    pub(super) window: Option<String>,
}

pub(super) async fn cost(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(query): Query<CostQuery>,
) -> Response {
    match served_cost(&state, runtime, &query).await {
        Ok(served) => (served.age_header(), Json((*served.body).clone())).into_response(),
        Err(response) => *response,
    }
}

/// The deprecated alias: the [`cost`] body plus a `deprecation` note, under
/// `Deprecation`, `Sunset` and successor `Link` headers.
pub(super) async fn scoreboard(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(query): Query<CostQuery>,
) -> Response {
    let deprecation_headers = [
        (HeaderName::from_static("deprecation"), "true"),
        (HeaderName::from_static("sunset"), SCOREBOARD_SUNSET),
        (header::LINK, "</api/cost>; rel=\"successor-version\""),
    ];
    match served_cost(&state, runtime, &query).await {
        Ok(served) => {
            let mut body = (*served.body).clone();
            body["deprecation"] = json!(SCOREBOARD_DEPRECATION);
            (served.age_header(), deprecation_headers, Json(body)).into_response()
        }
        Err(response) => (deprecation_headers, *response).into_response(),
    }
}

async fn served_cost(
    state: &DashboardState,
    runtime: Arc<OrbitRuntime>,
    query: &CostQuery,
) -> Result<Served<Value>, Box<Response>> {
    let window = match query.window.as_deref() {
        None => MetricsWindow::All,
        Some(raw) => raw.parse::<MetricsWindow>().map_err(|error| {
            Box::new(
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": error.to_string() })),
                )
                    .into_response(),
            )
        })?,
    };
    let runtime_for_compute = runtime.clone();
    state
        .cost_memo()
        .get_or_compute(
            &runtime,
            window.as_str().to_string(),
            DASHBOARD_POLL_MEMO_TTL,
            move || {
                let cost = runtime_for_compute.managed_execution_cost(window)?;
                serde_json::to_value(&cost)
                    .map_err(|e| orbit_core::OrbitError::Store(e.to_string()))
            },
        )
        .await
        .map_err(|error| Box::new(map_runtime_error(error)))
}
