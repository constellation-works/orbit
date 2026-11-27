//! Job catalog and job-run listing handlers.

use crate::state::WsCore;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_core::Core;
use orbit_core::app::run::{Command, exec};
use orbit_core::app::run::{
    JobFailureStreak, JobRunListParams, JobRunOrder, ResumeSubmissionProfile,
    ResumeSubmissionReceipt,
};
use orbit_types::workflow::JobRunState;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{OptionalJson, bad_request, blocking, bounded_limit, map_runtime_error, validate_id};
use orbit_core::app::run::job_catalog_to_json_with_last_run;

use super::routines::{OperationsQuery, explicit_workspace};

const JOB_RUN_DEFAULT_LIMIT: usize = 25;

/// Failure outcomes shared by the dashboard tile and run-list filters.
pub(super) const FAILED_RUN_STATES: [JobRunState; 3] = [
    JobRunState::Failed,
    JobRunState::Timeout,
    JobRunState::Interrupted,
];

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
    /// Include the dashboard's bounded running-run projection with an active
    /// count read, so both consumers share one HTTP request.
    #[serde(default)]
    include_running: bool,
    /// Duration (`24h`) or timestamp. Empty and `all` mean no time bound, so
    /// the dashboard window and `orbit run history --since` share one parser.
    #[serde(default)]
    since: Option<String>,
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

/// [ORB-10709] Optional body carrying the workspace claim token, for a resume
/// submitted while another operator holds the claim.
#[derive(Debug, Default, serde::Deserialize)]
pub(super) struct ResumeBody {
    #[serde(default)]
    claim_token: Option<String>,
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

/// Submit a catalog job in the selected workspace.
/// Delivery pipelines need task input and are deliberately unavailable through
/// this no-input action. The UI directs operators to Ship or Drain for those.
pub(super) async fn run_job_action(
    Query(query): Query<OperationsQuery>,
    WsCore(core): WsCore,
    Path(id): Path<String>,
) -> Response {
    if let Err(rejection) = explicit_workspace(&query) {
        return rejection.into_response();
    }
    match blocking("run job", move || {
        exec(&core, Command::submit_catalog_job(validate_id(&id))).map(|outcome| outcome.value)
    })
    .await
    {
        Ok(Err(message)) => bad_request(message),
        Ok(Ok(invoke)) => Json(json!({
            "job_id": invoke.job_name,
            "run_id": invoke.run_id,
            "state": if invoke.queued { "queued" } else { "submitted" },
            "submitted_at": invoke.submitted_at,
        }))
        .into_response(),
        Err(response) => *response,
    }
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

pub(super) async fn list_jobs(WsCore(core): WsCore) -> Response {
    match blocking("list jobs", move || {
        exec(&core, Command::catalog_with_streaks()).map(|outcome| outcome.value)
    })
    .await
    {
        Ok(orbit_core::app::run::JobCatalog { rows, streaks }) => {
            let streaks: std::collections::HashMap<_, _> = streaks
                .into_iter()
                .map(|streak| (streak.job_id.clone(), streak))
                .collect();
            let values: Vec<Value> = rows
                .iter()
                .map(|(entry, last_run)| {
                    let mut value = job_catalog_to_json_with_last_run(entry, last_run.as_ref());
                    value["failure_streak"] = streaks
                        .get(&entry.job_id)
                        .map_or(Value::Null, JobFailureStreak::to_json);
                    value
                })
                .collect();
            Json(Value::Array(values)).into_response()
        }
        Err(response) => *response,
    }
}

/// Jobs whose current failure streak has reached the fixed
/// configured failure-streak threshold, with that threshold. The dashboard reads this for the catalogue badges and
/// the Jobs rail count. Only manual-runnable jobs — the jobs the Catalogue
/// shows — are counted, so the badge stays consistent with what the operator
/// can see there; delivery and other non-runnable jobs' streaks do not count.
/// Display only.
pub(super) async fn list_job_failure_streaks(WsCore(core): WsCore) -> Response {
    match blocking("list job failure streaks", move || {
        exec(&core, Command::failure_streaks()).map(|outcome| outcome.value)
    })
    .await
    {
        Ok((threshold, failing)) => Json(json!({
            "threshold": threshold,
            "failing": failing.iter().map(JobFailureStreak::to_json).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(response) => *response,
    }
}

pub(super) async fn list_job_runs(
    WsCore(core): WsCore,
    Query(q): Query<JobRunListQuery>,
) -> Response {
    let limit = bounded_limit(q.limit, JOB_RUN_DEFAULT_LIMIT);
    let state = match JobRunListState::parse(q.state.as_deref()) {
        Ok(state) => state,
        Err(message) => return bad_request(message),
    };
    if q.include_running && !matches!(state, JobRunListState::Active) {
        return bad_request("include_running requires state=active".to_string());
    }
    let scope = match resolve_job_run_scope(
        q.job_id.as_deref(),
        q.task_id.as_deref(),
        q.since.as_deref(),
    ) {
        Ok(scope) => scope,
        Err(message) => return bad_request(message),
    };
    match blocking("list job runs", move || {
        job_runs_page(&core, &scope, state, limit, q.include_running)
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

fn job_runs_page(
    core: &Core,
    scope: &JobRunScope,
    state: JobRunListState,
    limit: usize,
    include_running: bool,
) -> Result<Value, orbit_common::OrbitError> {
    let params = job_run_list_params(scope, state, Some(limit));
    let running = include_running.then(|| {
        job_run_list_params(
            scope,
            JobRunListState::Concrete(JobRunState::Running),
            Some(100),
        )
    });
    let page_data = exec(core, Command::job_page(params, running))?.value;
    let mut page = json!({
        "items": page_data.items,
        "total": page_data.total,
        "limit": limit,
        "truncated": page_data.total > page_data.items.len() as u64,
        "state": state.label(),
    });
    if let Some(running) = page_data.running {
        page["running_items"] = Value::Array(running);
    }
    Ok(page)
}

/// The task detail's bounded Runs tab projection, shared with the task list
/// refresh when that tab is open.
pub(super) fn task_runs_page(
    core: &Core,
    task_id: &str,
    limit: usize,
) -> Result<Value, orbit_common::OrbitError> {
    let scope = JobRunScope {
        task_id: Some(task_id.to_string()),
        ..Default::default()
    };
    job_runs_page(core, &scope, JobRunListState::All, limit, false)
}

fn job_run_list_params(
    scope: &JobRunScope,
    filter: JobRunListState,
    limit: Option<usize>,
) -> JobRunListParams {
    let (state, states, terminal_only) = match filter {
        JobRunListState::All => (None, Vec::new(), false),
        JobRunListState::Concrete(state) => (Some(state), Vec::new(), false),
        JobRunListState::Terminal => (None, Vec::new(), true),
        JobRunListState::Failed => (None, FAILED_RUN_STATES.to_vec(), false),
        JobRunListState::Active => (
            None,
            vec![JobRunState::Pending, JobRunState::Running],
            false,
        ),
    };
    JobRunListParams {
        job_id: scope.job_id.clone(),
        task_id: scope.task_id.clone(),
        state,
        states,
        terminal_only,
        since: scope.since,
        limit,
        order_by: JobRunOrder::Recency,
    }
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
    WsCore(core): WsCore,
    Path(id): Path<String>,
    OptionalJson(body): OptionalJson<ResumeBody>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let id = id.to_string();
    let retry_source_run_id = id.clone();
    match blocking("resume run", move || {
        exec(
            &core,
            Command::resume_dashboard(&id, body.claim_token.as_deref()),
        )
        .map(|outcome| outcome.value)
    })
    .await
    {
        Ok(Ok(invoke)) => Json(
            ResumeSubmissionReceipt::new(
                &invoke,
                &retry_source_run_id,
                ResumeSubmissionProfile::Envelope,
            )
            .to_json(),
        )
        .into_response(),
        Ok(Err(error)) => match error.job_validation_message() {
            Some(message) => (StatusCode::CONFLICT, Json(json!({"error":message}))).into_response(),
            None => map_runtime_error(error),
        },
        Err(response) => *response,
    }
}
