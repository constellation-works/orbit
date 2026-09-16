//! `GET /api/metrics/reliability` — pipeline reliability across workspaces
//! [ORB-10588].
//!
//! Reports two rates the dashboard previously could not answer at all: how
//! often job runs fail, and how often the recovery path fires. Both come from
//! [`OrbitRuntime::pipeline_reliability`], which reads only persisted
//! `job_runs` / `invocations` rows — no token or cost field is on the path.
//!
//! This handler takes the whole [`DashboardState`] rather than the [`Ws`]
//! extractor, following [`super::workspaces`]: a failure spike is only
//! attributable if the operator can see which workspace it came from. Each
//! workspace is reported separately and a `totals` block sums the counts;
//! rates in `totals` are recomputed from the summed counts rather than
//! averaged, so a small workspace cannot skew the headline.
//!
//! [`Ws`]: crate::state::Ws
//! [`OrbitRuntime::pipeline_reliability`]: orbit_core::OrbitRuntime::pipeline_reliability

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::Utc;
use orbit_core::metrics::reliability::{
    BucketOutcomeRow, JobOutcomeRow, OutcomeCounts, Rate, ReliabilityWindow, RuntimeReliability,
};
use orbit_core::scoreboard_summary::ScoreboardWindow;
use serde::Deserialize;
use serde_json::{Value, json};

use super::blocking;
use crate::state::DashboardState;

/// Window used when `?window=` is omitted. Long enough that the rates carry a
/// usable `n` on a typical workspace, short enough to still be a current
/// reading rather than a lifetime average.
const DEFAULT_RELIABILITY_WINDOW: ScoreboardWindow = ScoreboardWindow::Week;

/// `?window=<1h|24h|7d|30d>`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct ReliabilityQuery {
    #[serde(default)]
    window: Option<String>,
}

pub(super) async fn pipeline_reliability(
    State(state): State<DashboardState>,
    Query(query): Query<ReliabilityQuery>,
) -> Response {
    let window = match resolve_window(query.window.as_deref()) {
        Ok(window) => window,
        Err(message) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response();
        }
    };

    match blocking("pipeline reliability", move || {
        pipeline_reliability_json(&state, window)
    })
    .await
    {
        Ok(payload) => Json(payload).into_response(),
        Err(response) => *response,
    }
}

/// Pin one snapshot and read each workspace's reliability on the blocking pool
/// so SQLite scans cannot park a tokio worker.
fn pipeline_reliability_json(
    state: &DashboardState,
    window: ReliabilityWindow,
) -> Result<Value, orbit_core::OrbitError> {
    let pinned = state.pin();
    let mut workspaces = Vec::new();
    let mut totals = OutcomeCounts::default();
    let mut recovery_numerator = 0_u64;
    let mut recovery_denominator = 0_u64;
    let mut runs_numerator = 0_u64;
    let mut runs_denominator = 0_u64;
    let mut truncated = false;
    let mut unreadable = Vec::new();

    for entry in pinned.entries().iter().filter(|entry| entry.active) {
        let Ok(runtime) = pinned.runtime_for(&entry.id) else {
            // A workspace whose runtime will not open is named rather than
            // dropped: silently omitting it would understate every total.
            unreadable.push(entry.id.clone());
            continue;
        };
        let reliability = runtime.pipeline_reliability(&window)?;

        accumulate(&mut totals, &reliability.job_runs.overall);
        recovery_numerator += reliability.recovery.per_step_invocation.numerator;
        recovery_denominator += reliability.recovery.per_step_invocation.denominator;
        runs_numerator += reliability.recovery.per_job_run.numerator;
        runs_denominator += reliability.recovery.per_job_run.denominator;
        truncated |= reliability.job_runs.truncated;

        workspaces.push(workspace_value(&entry.id, &entry.name, &reliability));
    }

    Ok(json!({
        "window": window,
        // ORB-10872: this endpoint is intentionally fleet-wide (ORB-10588).
        // The dashboard must label that exception rather than inherit the
        // header workspace selector.
        "scope": "fleet",
        "workspaces": workspaces,
        "totals": {
            "job_runs": {
                "counts": totals,
                "failure_rate": totals.failure_rate(),
                "truncated": truncated,
            },
            "recovery": {
                "per_step_invocation": Rate::new(
                    recovery_numerator,
                    recovery_denominator,
                    "step-activity invocations",
                ),
                "per_job_run": Rate::new(
                    runs_numerator,
                    runs_denominator,
                    "job runs with any recorded invocation",
                ),
            },
        },
        "unreadable_workspaces": unreadable,
    }))
}

/// Maps the shared dashboard window vocabulary onto a [`ReliabilityWindow`].
///
/// `all` is rejected: a rate with no stated time range is not actionable, and
/// a lifetime average would hide exactly the spikes this view exists to
/// surface.
fn resolve_window(raw: Option<&str>) -> Result<ReliabilityWindow, String> {
    let selected = match raw {
        None => DEFAULT_RELIABILITY_WINDOW,
        Some(value) => value
            .parse::<ScoreboardWindow>()
            .map_err(|error| error.to_string())?,
    };
    let Some(duration) = selected.duration() else {
        return Err("window must be one of: 1h, 24h, 7d, 30d (reliability rates require an explicit time range)".to_string());
    };
    Ok(ReliabilityWindow::ending_at(
        selected.as_str(),
        Utc::now(),
        duration,
    ))
}

fn accumulate(totals: &mut OutcomeCounts, counts: &OutcomeCounts) {
    totals.total += counts.total;
    totals.succeeded += counts.succeeded;
    totals.failed += counts.failed;
    totals.cancelled += counts.cancelled;
    totals.skipped += counts.skipped;
    totals.in_flight += counts.in_flight;
    totals.unknown += counts.unknown;
}

/// Shapes one workspace's block, attaching each scope's derived failure rate
/// next to the counts it came from so the frontend never has to divide.
fn workspace_value(id: &str, name: &str, reliability: &RuntimeReliability) -> Value {
    json!({
        "workspace_id": id,
        "workspace_name": name,
        "job_runs": {
            "counts": reliability.job_runs.overall,
            "failure_rate": reliability.job_runs.overall.failure_rate(),
            "by_job": reliability
                .job_runs
                .by_job
                .iter()
                .map(job_row_value)
                .collect::<Vec<_>>(),
            "over_time": reliability
                .job_runs
                .over_time
                .iter()
                .map(bucket_row_value)
                .collect::<Vec<_>>(),
            "observed_states": reliability.job_runs.observed_states,
            "truncated": reliability.job_runs.truncated,
        },
        "recovery": reliability.recovery,
    })
}

fn job_row_value(row: &JobOutcomeRow) -> Value {
    json!({
        "job_id": row.job_id,
        "counts": row.counts,
        "failure_rate": row.counts.failure_rate(),
    })
}

fn bucket_row_value(row: &BucketOutcomeRow) -> Value {
    json!({
        "bucket_start": row.bucket_start,
        "bucket_end": row.bucket_end,
        "counts": row.counts,
        "failure_rate": row.counts.failure_rate(),
    })
}
