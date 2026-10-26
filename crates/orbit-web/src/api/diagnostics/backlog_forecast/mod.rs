//! HTTP adapter for the shared backlog forecast and calibration service.

use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

use crate::state::Ws;

#[derive(Deserialize, Default)]
pub(in crate::api) struct BacklogForecastQuery {
    /// History lookback in days (7-180, default 30).
    #[serde(default)]
    lookback_days: Option<i64>,
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
        runtime.backlog_forecast(q.lookback_days, q.concurrency, q.arrivals.as_deref())
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}
