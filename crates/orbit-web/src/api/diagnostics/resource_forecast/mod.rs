//! `GET /api/diagnostics/resource_forecast`: Analytics › Forecast › Resource
//! forecast. How well this host's admission forecasts matched its measured
//! load, and what each drain's shadow and enforce passes decided: the report
//! `orbit run profiles --accuracy --json` prints over its default selection
//! (the newest 5,000 forecasts, whatever their age), beside the admission
//! settings and host size it was judged against.
//!
//! The per-pair rows are left out: the dashboard shows the per-offset
//! summaries, and every pair would make the answer grow with the history.
//! Read-only; profiles come from `/api/resource-profiles` and the live pass
//! from drain readiness.

use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_core::runtime::host_resource::accuracy::{
    AccuracySelection, DrainLimiters, DrainShadowDecisions, OffsetAccuracy,
};
use orbit_core::runtime::host_resource::forecast::HostCapacity;
use serde::Serialize;

use crate::state::Ws;

pub(in crate::api) async fn diagnostics_resource_forecast(Ws(runtime): Ws) -> Response {
    match super::super::blocking("resource forecast", move || {
        let workspace_id = runtime.workspace_id()?;
        let settings = runtime.auto_concurrency_settings();
        let report = runtime.forecast_accuracy_selected(AccuracySelection::latest(), Utc::now())?;
        serde_json::to_value(ResourceForecastBody {
            workspace_id: &workspace_id,
            configured_mode: &settings.mode,
            host: runtime.host_capacity(),
            admit_cpu_percent: settings.admit_cpu_percent,
            admit_memory_percent: orbit_config::AUTO_CONCURRENCY_ADMIT_MEMORY_PERCENT,
            min_profile_deliveries: settings.min_profile_deliveries,
            horizon_cap_minutes: orbit_config::AUTO_CONCURRENCY_HORIZON_CAP_MINUTES,
            accuracy: AccuracyBody {
                since: report.since,
                report_at: report.report_at,
                selection: &report.selection,
                forecasts: report.forecasts,
                offsets: &report.offsets,
                drains: &report.drains,
                limiters: &report.limiters,
            },
        })
        .map_err(|error| {
            orbit_core::OrbitError::Execution(format!("encode resource forecast: {error}"))
        })
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

#[derive(Serialize)]
struct ResourceForecastBody<'a> {
    workspace_id: &'a str,
    /// `machine.auto_concurrency.mode` on this host; a drain started with an
    /// explicit mode keeps it (drain readiness names that drain's own).
    configured_mode: &'a str,
    host: Option<HostCapacity>,
    admit_cpu_percent: u8,
    admit_memory_percent: u8,
    min_profile_deliveries: u32,
    horizon_cap_minutes: u32,
    accuracy: AccuracyBody<'a>,
}

/// `ForecastAccuracy` without its per-pair rows.
#[derive(Serialize)]
struct AccuracyBody<'a> {
    since: DateTime<Utc>,
    report_at: DateTime<Utc>,
    selection: &'a AccuracySelection,
    forecasts: u64,
    offsets: &'a [OffsetAccuracy],
    drains: &'a [DrainShadowDecisions],
    limiters: &'a [DrainLimiters],
}
