//! Job catalog and job-run listing handlers.

use crate::state::Ws;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_common::governance::authorization::{DASHBOARD_AUTO_DRAIN_COMPLETE, DASHBOARD_JOB_RUN};
use orbit_core::application::job::{JobRunListParams, JobRunOrder, job_run_to_json};
use orbit_core::{JobRun, JobRunState, OrbitRuntime};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{OptionalJson, bad_request, blocking, bounded_limit, map_runtime_error, validate_id};
use crate::projections::job_catalog_to_json_with_last_run;
use crate::state::DashboardState;

use super::routines::{
    OperationsQuery, authorization_denied, authorized_caller, explicit_workspace,
};

const JOB_RUN_DEFAULT_LIMIT: usize = 25;

/// Failure outcomes shared by the dashboard tile and run-list filters.
pub(super) const FAILED_RUN_STATES: [JobRunState; 3] = [
    JobRunState::Failed,
    JobRunState::Timeout,
    JobRunState::Interrupted,
];

/// Submit a catalog job in the selected workspace.
/// Delivery pipelines need task input and are deliberately unavailable through
/// this no-input action. The UI directs operators to Ship or Drain for those.
pub(super) async fn run_job_action(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
) -> Response {
    if let Err(rejection) = explicit_workspace(&query) {
        return rejection.into_response();
    }
    if let Err(denial) = authorized_caller(&DASHBOARD_JOB_RUN, state.operator_session()) {
        return authorization_denied(denial);
    }
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    match blocking("run job", move || {
        runtime.submit_no_input_catalog_job_run(
            &id,
            Some("dashboard"),
            orbit_types::workflow::JobRunTrigger::dashboard(),
        )
    })
    .await
    {
        Ok(invoke) => Json(json!({
            "job_id": invoke.job_name,
            "run_id": invoke.run_id,
            "state": if invoke.queued { "queued" } else { "submitted" },
            "submitted_at": invoke.submitted_at,
        }))
        .into_response(),
        Err(response) => *response,
    }
}

#[derive(Deserialize, Default)]
pub(super) struct JobRunListQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    job_id: Option<String>,
    /// Exact task id. Matches `input.task_ids` membership or a text top-level
    /// `input.task_id`, the same rule as `orbit run history --task`.
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    state: Option<String>,
    /// Duration (`24h`) or timestamp. Empty and `all` mean no time bound, so
    /// the dashboard window and `orbit run history --since` share one parser.
    #[serde(default)]
    since: Option<String>,
}

/// Parsed run-list filters shared by the workspace list and the aggregate list.
///
/// `since` is already resolved to one cutoff. `None` means the list is not
/// time-bounded, which is what the dashboard's `all` window asks for.
#[derive(Clone, Default)]
pub(super) struct JobRunScope {
    pub(super) job_id: Option<String>,
    pub(super) task_id: Option<String>,
    pub(super) since: Option<DateTime<Utc>>,
}

fn trimmed_filter(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Resolve job, task, and window filters once so a page and its count share
/// one cutoff. An unparseable `since` is refused before either query runs.
pub(super) fn resolve_job_run_scope(
    job_id: Option<&str>,
    task_id: Option<&str>,
    since: Option<&str>,
) -> Result<JobRunScope, String> {
    let since = match trimmed_filter(since) {
        None => None,
        Some(value) if value.eq_ignore_ascii_case("all") => None,
        Some(value) => Some(crate::parse::parse_since(&value).map_err(|error| error.to_string())?),
    };
    Ok(JobRunScope {
        job_id: trimmed_filter(job_id),
        task_id: trimmed_filter(task_id),
        since,
    })
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
    match blocking("list jobs", move || {
        runtime.list_job_catalog_with_last_run(true, JobCatalogFilter::All)
    })
    .await
    {
        Ok(rows) => {
            let values: Vec<Value> = rows
                .iter()
                .map(|(entry, last_run)| {
                    job_catalog_to_json_with_last_run(entry, last_run.as_ref())
                })
                .collect();
            Json(Value::Array(values)).into_response()
        }
        Err(response) => *response,
    }
}

pub(super) async fn list_job_runs(Ws(runtime): Ws, Query(q): Query<JobRunListQuery>) -> Response {
    let limit = bounded_limit(q.limit, JOB_RUN_DEFAULT_LIMIT);
    let state = match JobRunListState::parse(q.state.as_deref()) {
        Ok(state) => state,
        Err(message) => return bad_request(message),
    };
    let scope = match resolve_job_run_scope(
        q.job_id.as_deref(),
        q.task_id.as_deref(),
        q.since.as_deref(),
    ) {
        Ok(scope) => scope,
        Err(message) => return bad_request(message),
    };
    match blocking("list job runs", move || {
        job_runs_page(&runtime, &scope, state, limit)
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

fn job_runs_page(
    runtime: &OrbitRuntime,
    scope: &JobRunScope,
    state: JobRunListState,
    limit: usize,
) -> Result<Value, orbit_core::OrbitError> {
    let runs = list_job_runs_for_state(runtime, scope, state, limit)?;
    let total = count_job_runs_for_state(runtime, scope, state)?;
    let truncated = total > runs.len() as u64;
    let titles = super::run_tasks::task_titles(runtime, &runs)?;
    let items: Vec<Value> = runs
        .iter()
        .map(|run| {
            let mut value = job_run_to_json(run, None);
            super::run_tasks::add_tasks(&mut value, run, &titles);
            value
        })
        .collect();
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
    scope: &JobRunScope,
    state: JobRunListState,
    limit: usize,
) -> Result<Vec<JobRun>, orbit_core::OrbitError> {
    let list = |run_state, terminal_only| {
        runtime.list_job_runs(job_run_list_params(
            scope,
            run_state,
            terminal_only,
            Some(limit),
        ))
    };
    let states: &[JobRunState] = match state {
        JobRunListState::All => return list(None, false),
        JobRunListState::Concrete(run_state) => return list(Some(run_state), false),
        JobRunListState::Terminal => return list(None, true),
        JobRunListState::Failed => &FAILED_RUN_STATES,
        JobRunListState::Active => &[JobRunState::Pending, JobRunState::Running],
    };
    let mut runs = Vec::new();
    for &run_state in states {
        runs.extend(list(Some(run_state), false)?);
    }
    runs.sort_by(|left, right| {
        job_run_timestamp(right)
            .cmp(&job_run_timestamp(left))
            .then_with(|| left.run_id.cmp(&right.run_id))
    });
    runs.truncate(limit);
    Ok(runs)
}

fn count_job_runs_for_state(
    runtime: &OrbitRuntime,
    scope: &JobRunScope,
    state: JobRunListState,
) -> Result<u64, orbit_core::OrbitError> {
    let count = |run_state, terminal_only| {
        runtime.count_job_runs(job_run_list_params(scope, run_state, terminal_only, None))
    };
    let states: &[JobRunState] = match state {
        JobRunListState::All => return count(None, false),
        JobRunListState::Concrete(run_state) => return count(Some(run_state), false),
        JobRunListState::Terminal => return count(None, true),
        JobRunListState::Failed => &FAILED_RUN_STATES,
        JobRunListState::Active => &[JobRunState::Pending, JobRunState::Running],
    };
    let mut total = 0_u64;
    for &run_state in states {
        total = total.saturating_add(count(Some(run_state), false)?);
    }
    Ok(total)
}

fn job_run_list_params(
    scope: &JobRunScope,
    state: Option<JobRunState>,
    terminal_only: bool,
    limit: Option<usize>,
) -> JobRunListParams {
    JobRunListParams {
        job_id: scope.job_id.clone(),
        task_id: scope.task_id.clone(),
        state,
        terminal_only,
        since: scope.since,
        limit,
        order_by: JobRunOrder::Recency,
        ..Default::default()
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
/// Inherited automatic completion requires the current session's authority.
///
/// [ORB-10470] One-shot, like `POST /workflows/ship`: it returns as soon as the
/// resumed run is persisted and its detached worker is spawned, so the resumed
/// pipeline never runs on a request thread. Callers poll `/job-runs/:id` for
/// progress and can cancel the returned run id while it executes.
pub(super) async fn resume_job_run_action(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
    OptionalJson(body): OptionalJson<ResumeBody>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let id = id.to_string();
    let retry_source_run_id = id.clone();
    let completion_authority =
        authorized_caller(&DASHBOARD_AUTO_DRAIN_COMPLETE, state.operator_session());
    match blocking("resume run", move || {
        let source = runtime.show_job_run(&id)?;
        if source
            .input
            .as_ref()
            .and_then(|input| input.get("completion"))
            .and_then(Value::as_str)
            == Some("done")
            && let Err(denial) = completion_authority
        {
            return Ok(Err(authorization_denied(denial)));
        }
        Ok(runtime
            .submit_resume_run(&id, Some("dashboard"), body.claim_token.as_deref())
            .map_err(|error| match error {
                orbit_core::OrbitError::JobValidation(message) => {
                    (StatusCode::CONFLICT, Json(json!({ "error": message }))).into_response()
                }
                other => map_runtime_error(other),
            }))
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
        Ok(Err(response)) => response,
        Err(response) => *response,
    }
}
