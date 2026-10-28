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
use orbit_core::metrics::cost::MetricsWindow;
use orbit_core::metrics::reliability::{
    BucketOutcomeRow, JobOutcomeRow, OutcomeCounts, Rate, ReliabilityWindow, RuntimeReliability,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::blocking;
use crate::state::DashboardState;

/// Window used when `?window=` is omitted. Long enough that the rates carry a
/// usable `n` on a typical workspace, short enough to still be a current
/// reading rather than a lifetime average.
const DEFAULT_RELIABILITY_WINDOW: MetricsWindow = MetricsWindow::Week;

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

/// Upper bound on the scoped threads one reliability request fans out to.
/// Each workspace read is independent (its own runtime and store), so the
/// aggregate waits for the slowest workspace instead of the sum; the bound
/// keeps a host with many registered workspaces from spawning a thread per
/// workspace per poll. Matches the fleet drains/tasks aggregates.
const MAX_RELIABILITY_FAN_OUT: usize = 8;

/// Run `work(index)` for every index in `0..count` on up to
/// [`MAX_RELIABILITY_FAN_OUT`] scoped threads, returning the results in
/// index order. A panicking worker re-raises in the caller, exactly as the
/// serial loop would.
fn fan_out<T>(count: usize, work: impl Fn(usize) -> T + Sync) -> Vec<T>
where
    T: Send,
{
    if count <= 1 {
        return (0..count).map(work).collect();
    }
    let workers = std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(4)
        .min(MAX_RELIABILITY_FAN_OUT)
        .min(count);
    let chunk = count.div_ceil(workers);
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for start in (0..count).step_by(chunk) {
            let end = (start + chunk).min(count);
            let work = &work;
            handles.push(scope.spawn(move || (start..end).map(work).collect::<Vec<_>>()));
        }
        let mut results = Vec::with_capacity(count);
        for handle in handles {
            match handle.join() {
                Ok(chunk_results) => results.extend(chunk_results),
                Err(payload) => std::panic::resume_unwind(payload),
            }
        }
        results
    })
}

/// One workspace's fleet-reliability read: either skipped (inactive), an
/// unreadable id, or the reliability payload to fold in order.
enum WorkspaceReliability {
    Skipped,
    Unreadable(String),
    Read {
        id: String,
        name: String,
        // Boxed: the unboxed payload trips `large_enum_variant` (one heap
        // allocation per workspace is noise next to its five store queries).
        reliability: Box<RuntimeReliability>,
    },
}

/// Pin one snapshot and read each workspace's reliability on the blocking pool
/// so SQLite scans cannot park a tokio worker.
fn pipeline_reliability_json(
    state: &DashboardState,
    window: ReliabilityWindow,
) -> Result<Value, orbit_core::OrbitError> {
    let pinned = state.pin();
    let aggregate_start = std::time::Instant::now();
    // Resolve and query every workspace concurrently (its own runtime and
    // store); the fold below runs in pinned entry order, so `workspaces`,
    // `totals` and `unreadable_workspaces` keep their serial order and the
    // single pinned snapshot stays the authority for every read.
    let entries = pinned.entries();
    let reads = fan_out(entries.len(), |index| {
        let entry = &entries[index];
        if !entry.active {
            return WorkspaceReliability::Skipped;
        }
        let query_start = std::time::Instant::now();
        let runtime = match pinned.runtime_for(&entry.id) {
            Ok(runtime) => runtime,
            // A workspace whose runtime will not open is named rather than
            // dropped: silently omitting it would understate every total.
            Err(error) => {
                tracing::warn!(workspace = %entry.id, error = ?error, "fleet reliability could not open a workspace");
                return WorkspaceReliability::Unreadable(entry.id.clone());
            }
        };
        // Same for one whose query fails: failing the whole fleet view would
        // hide every healthy workspace behind the broken one.
        match runtime.pipeline_reliability(&window) {
            Ok(reliability) => {
                let elapsed_ms = query_start.elapsed().as_millis();
                tracing::debug!(
                    workspace = %entry.id,
                    elapsed_ms,
                    "fleet reliability read a workspace"
                );
                WorkspaceReliability::Read {
                    id: entry.id.clone(),
                    name: entry.name.clone(),
                    reliability: Box::new(reliability),
                }
            }
            Err(error) => {
                tracing::warn!(workspace = %entry.id, %error, "fleet reliability skipped a workspace");
                WorkspaceReliability::Unreadable(entry.id.clone())
            }
        }
    });

    let mut workspaces = Vec::new();
    let mut totals = OutcomeCounts::default();
    let mut recovery_numerator = 0_u64;
    let mut recovery_denominator = 0_u64;
    let mut runs_numerator = 0_u64;
    let mut runs_denominator = 0_u64;
    let mut truncated = false;
    let mut unreadable = Vec::new();

    for read in reads {
        let (id, name, reliability) = match read {
            WorkspaceReliability::Skipped => continue,
            WorkspaceReliability::Unreadable(id) => {
                unreadable.push(id);
                continue;
            }
            WorkspaceReliability::Read {
                id,
                name,
                reliability,
                ..
            } => (id, name, reliability),
        };

        accumulate(&mut totals, &reliability.job_runs.overall);
        recovery_numerator += reliability.recovery.per_step_invocation.numerator;
        recovery_denominator += reliability.recovery.per_step_invocation.denominator;
        runs_numerator += reliability.recovery.per_job_run.numerator;
        runs_denominator += reliability.recovery.per_job_run.denominator;
        truncated |= reliability.job_runs.truncated;

        workspaces.push(workspace_value(&id, &name, &reliability));
    }
    tracing::debug!(
        workspaces = workspaces.len(),
        unreadable = unreadable.len(),
        elapsed_ms = aggregate_start.elapsed().as_millis(),
        "fleet reliability aggregated workspaces"
    );

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
            .parse::<MetricsWindow>()
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
    totals.cascaded += counts.cascaded;
    totals.cancelled += counts.cancelled;
    totals.skipped += counts.skipped;
    totals.held += counts.held;
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
