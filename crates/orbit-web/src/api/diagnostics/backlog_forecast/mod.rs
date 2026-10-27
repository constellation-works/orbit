//! HTTP adapter for the shared backlog forecast and calibration service.

use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

use crate::state::Ws;
use orbit_core::application::task::BacklogForecastHistory;

#[derive(Deserialize, Default)]
pub(in crate::api) struct BacklogForecastQuery {
    /// History lookback in days (7-180, default 30), reaching back to at
    /// least the newest 50 completions.
    #[serde(default)]
    lookback_days: Option<i64>,
    /// History as the newest this many completions (1-5000), whatever their
    /// age, instead of `lookback_days`.
    #[serde(default)]
    history_tasks: Option<usize>,
    /// Leaf runs to simulate; the live drain's ceiling when absent.
    #[serde(default)]
    concurrency: Option<u32>,
    /// `on` adds new tasks at the recent arrival rate.
    #[serde(default)]
    arrivals: Option<String>,
}

pub(in crate::api) async fn diagnostics_backlog_forecast(
    Ws(runtime): Ws,
    Query(q): Query<BacklogForecastQuery>,
) -> Response {
    match super::super::blocking("backlog forecast", move || {
        runtime.backlog_forecast(
            BacklogForecastHistory {
                lookback_days: q.lookback_days,
                history_tasks: q.history_tasks,
            },
            q.concurrency,
            q.arrivals.as_deref(),
        )
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}
