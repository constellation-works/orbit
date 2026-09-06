//! Job catalog and job-run listing handlers.

use crate::state::Ws;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_core::JobRunState;
use orbit_core::application::job::{JobRunListParams, job_run_to_json};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{bad_request, bounded_limit, map_runtime_error, server_error, validate_id};
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

enum JobRunListState {
    Concrete(JobRunState),
    Terminal,
}

impl JobRunListState {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Concrete(JobRunState::Pending)),
            "running" => Ok(Self::Concrete(JobRunState::Running)),
            "terminal" => Ok(Self::Terminal),
            _ => Err("invalid state; expected one of: pending, running, terminal".to_string()),
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
    let state = match q.state.as_deref().map(JobRunListState::parse).transpose() {
        Ok(state) => state,
        Err(message) => return bad_request(message),
    };
    let params = JobRunListParams {
        job_id: q.job_id,
        state: state.as_ref().and_then(|state| match state {
            JobRunListState::Concrete(state) => Some(*state),
            JobRunListState::Terminal => None,
        }),
        terminal_only: matches!(state, Some(JobRunListState::Terminal)),
        since: q.since,
        limit: Some(limit),
        ..Default::default()
    };
    match runtime.list_job_runs(params) {
        Ok(runs) => {
            let values: Vec<Value> = runs.iter().map(|run| job_run_to_json(run, None)).collect();
            Json(Value::Array(values)).into_response()
        }
        Err(e) => server_error(e),
    }
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
    match runtime.submit_resume_run(id, Some("dashboard"), body.claim_token.as_deref()) {
        Ok(invoke) => Json(json!({
            "workflow": "resume",
            "job_id": invoke.job_name,
            "run_id": invoke.run_id,
            "retry_source_run_id": id,
            "state": if invoke.queued { "queued" } else { "submitted" },
            "submitted_at": invoke.submitted_at,
        }))
        .into_response(),
        Err(orbit_core::OrbitError::JobValidation(message)) => {
            (StatusCode::CONFLICT, Json(json!({ "error": message }))).into_response()
        }
        Err(e) => map_runtime_error(e),
    }
}
