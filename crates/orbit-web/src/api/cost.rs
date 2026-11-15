//! `GET /api/cost`: managed-execution cost and token usage for the dashboard
//! Cost page.

use crate::runtime_memo::{DASHBOARD_POLL_MEMO_TTL, Served};
use crate::state::{DashboardState, Ws};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::OrbitRuntime;
use orbit_core::metrics::cost::MetricsWindow;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

use super::map_runtime_error;

/// Query-string shape for `GET /api/cost`.
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
                let body = serde_json::to_value(&cost)
                    .map_err(|e| orbit_core::OrbitError::Store(e.to_string()))?;
                Ok(body)
            },
        )
        .await
        .map_err(|error| Box::new(map_runtime_error(error)))
}
