//! Job catalog and job-run listing handlers.

use crate::state::Ws;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_core::application::job::{JobRunListParams, JobRunOrder, job_run_to_json};
use orbit_core::{JobRun, JobRunState, OrbitRuntime};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{bad_request, blocking, bounded_limit, map_runtime_error, server_error, validate_id};
use crate::projections::job_catalog_to_json_with_last_run;

const JOB_RUN_DEFAULT_LIMIT: usize = 25;

#[derive(Deserialize, Default)]
pub(super) struct JobRunListQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    since: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy)]
enum JobRunListState {
    All,
    Active,
    Failed,
    Concrete(JobRunState),
    Terminal,
}

impl JobRunListState {
    fn parse(value: Option<&str>) -> Result<Self, String> {
        match value {
            None | Some("all") => Ok(Self::All),
            Some("active") => Ok(Self::Active),
            Some("failed") => Ok(Self::Failed),
            Some("pending") => Ok(Self::Concrete(JobRunState::Pending)),
            Some("running") => Ok(Self::Concrete(JobRunState::Running)),
            Some("terminal") => Ok(Self::Terminal),
            Some(_) => Err(
                "invalid state; expected one of: all, active, failed, pending, running, terminal"
                    .to_string(),
            ),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Active => "active",
            Self::Failed => "failed",
            Self::Concrete(JobRunState::Pending) => "pending",
            Self::Concrete(JobRunState::Running) => "running",
            Self::Concrete(_) => "concrete",
            Self::Terminal => "terminal",
        }
    }
}

pub(super) async fn list_jobs(Ws(runtime): Ws) -> Response {
    use orbit_core::application::job::JobCatalogFilter;
    match runtime.list_job_catalog_with_last_run(true, JobCatalogFilter::All) {
        Ok(rows) => {
            let values: Vec<Value> = rows
                .iter()
                .map(|(entry, last_run)| {
                    job_catalog_to_json_with_last_run(entry, last_run.as_ref())
                })
                .collect();
            Json(Value::Array(values)).into_response()
        }
        Err(e) => server_error(e),
    }
}

pub(super) async fn list_job_runs(Ws(runtime): Ws, Query(q): Query<JobRunListQuery>) -> Response {
    let limit = bounded_limit(q.limit, JOB_RUN_DEFAULT_LIMIT);
    let state = match JobRunListState::parse(q.state.as_deref()) {
        Ok(state) => state,
        Err(message) => return bad_request(message),
    };
    match job_runs_page(&runtime, &q, state, limit) {
        Ok(value) => Json(value).into_response(),
        Err(e) => server_error(e),
    }
}

fn job_runs_page(
    runtime: &OrbitRuntime,
    query: &JobRunListQuery,
    state: JobRunListState,
    limit: usize,
) -> Result<Value, orbit_core::OrbitError> {
    let runs = list_job_runs_for_state(runtime, query, state, limit)?;
    let total = count_job_runs_for_state(runtime, query, state)?;
    let truncated = total > runs.len() as u64;
    let items: Vec<Value> = runs.iter().map(|run| job_run_to_json(run, None)).collect();
    Ok(json!({
        "items": items,
        "total": total,
        "limit": limit,
        "truncated": truncated,
        "state": state.label(),
    }))
}

fn list_job_runs_for_state(
    runtime: &OrbitRuntime,
    query: &JobRunListQuery,
    state: JobRunListState,
    limit: usize,
) -> Result<Vec<JobRun>, orbit_core::OrbitError> {
    let list = |run_state, terminal_only| {
        runtime.list_job_runs(job_run_list_params(
            query,
            run_state,
            terminal_only,
            Some(limit),
        ))
    };
    match state {
        JobRunListState::All => list(None, false),
        JobRunListState::Failed => list(Some(JobRunState::Failed), false),
        JobRunListState::Concrete(run_state) => list(Some(run_state), false),
        JobRunListState::Terminal => list(None, true),
        JobRunListState::Active => {
            let mut runs = list(Some(JobRunState::Pending), false)?;
            runs.extend(list(Some(JobRunState::Running), false)?);
            runs.sort_by(|left, right| {
                job_run_timestamp(right)
                    .cmp(&job_run_timestamp(left))
                    .then_with(|| left.run_id.cmp(&right.run_id))
            });
            runs.truncate(limit);
            Ok(runs)
        }
    }
}

fn count_job_runs_for_state(
    runtime: &OrbitRuntime,
    query: &JobRunListQuery,
    state: JobRunListState,
) -> Result<u64, orbit_core::OrbitError> {
    let count = |run_state, terminal_only| {
        runtime.count_job_runs(job_run_list_params(query, run_state, terminal_only, None))
    };
    match state {
        JobRunListState::All => count(None, false),
        JobRunListState::Failed => count(Some(JobRunState::Failed), false),
        JobRunListState::Concrete(run_state) => count(Some(run_state), false),
        JobRunListState::Terminal => count(None, true),
        JobRunListState::Active => {
            let pending = count(Some(JobRunState::Pending), false)?;
            let running = count(Some(JobRunState::Running), false)?;
            Ok(pending.saturating_add(running))
        }
    }
}

fn job_run_list_params(
    query: &JobRunListQuery,
    state: Option<JobRunState>,
    terminal_only: bool,
    limit: Option<usize>,
) -> JobRunListParams {
    JobRunListParams {
        job_id: query.job_id.clone(),
        state,
        terminal_only,
        since: query.since,
        limit,
        order_by: JobRunOrder::Recency,
    }
}

fn job_run_timestamp(run: &JobRun) -> DateTime<Utc> {
    run.finished_at.or(run.started_at).unwrap_or(run.created_at)
}

/// [ORB-10709] Optional body carrying the workspace claim token, for a resume
/// submitted while another operator holds the claim.
#[derive(Debug, Default, serde::Deserialize)]
pub(super) struct ResumeBody {
    #[serde(default)]
    claim_token: Option<String>,
}

/// Submit a resume of a terminal resumable run as a new linked run.
///
/// Resume re-runs the first non-successful step and every subsequent step; it
/// succeeds only when the underlying cause of the source failure is resolved.
///
/// [ORB-10470] One-shot, like `POST /workflows/ship`: it returns as soon as the
/// resumed run is persisted and its detached worker is spawned, so the resumed
/// pipeline never runs on a request thread. Callers poll `/job-runs/:id` for
/// progress and can cancel the returned run id while it executes.
pub(super) async fn resume_job_run_action(
    Ws(runtime): Ws,
    Path(id): Path<String>,
    body: Option<Json<ResumeBody>>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let Json(body) = body.unwrap_or_default();
    let id = id.to_string();
    let retry_source_run_id = id.clone();
    match blocking("resume run", move || {
        Ok(runtime.submit_resume_run(&id, Some("dashboard"), body.claim_token.as_deref()))
    })
    .await
    {
        Ok(Ok(invoke)) => Json(json!({
            "workflow": "resume",
            "job_id": invoke.job_name,
            "run_id": invoke.run_id,
            "retry_source_run_id": retry_source_run_id,
            "state": if invoke.queued { "queued" } else { "submitted" },
            "submitted_at": invoke.submitted_at,
        }))
        .into_response(),
        Ok(Err(orbit_core::OrbitError::JobValidation(message))) => {
            (StatusCode::CONFLICT, Json(json!({ "error": message }))).into_response()
        }
        Ok(Err(e)) => map_runtime_error(e),
        Err(response) => *response,
    }
}
