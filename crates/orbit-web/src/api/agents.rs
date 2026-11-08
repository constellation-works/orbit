//! `GET /api/agents/{role}/scorecard`, `/api/agents/{role}/detail`,
//! `/api/agents/attribution`, the implementer slices
//! (`/api/agents/implementers/by-type` and `/size-curves`) and the model
//! ranking (`/api/agents/rank`): the agent scorecards behind Analytics ›
//! Agents. `/api/agents/ci-failures` reads the required-check failures
//! recorded by cause live, without the cached scan.
//!
//! The scan, attribution and scores are the core agent scorecard service.
//! One scan per workspace runtime is cached for [`AGENT_SCAN_TTL`]; the role,
//! window, minimum support and detail only slice the cached facts, so
//! changing them or polling inside the cache period never rescans task
//! bundles. A stale scan is served while one background refresh replaces it.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::application::agents::{
    self, AgentFacts, AgentQuery, AgentRole, CiFailureQuery, RankQuery,
};
use orbit_core::{OrbitError, OrbitRuntime};
use serde::Deserialize;

use super::{bad_request, blocking, map_runtime_error};
use crate::runtime_memo::{AGENT_SCAN_STALE_LIMIT, AGENT_SCAN_TTL, Served};
use crate::state::{DashboardState, Ws};

/// Query string for the agent scorecard routes.
#[derive(Debug, Default, Deserialize)]
pub(super) struct AgentScorecardQuery {
    /// `1h`, `24h`, `7d`, `30d` (default), `90d` or `all` (served as `90d`).
    #[serde(default)]
    window: Option<String>,
    /// Units a row needs before it is ranked (default 10, at least 1).
    #[serde(default)]
    min_support: Option<usize>,
    /// The row's crew, or for orchestrators the orchestrator; `/detail` only,
    /// required there.
    #[serde(default)]
    crew: Option<String>,
    /// The row's model, `model unknown` for none; `/detail` only, optional
    /// when the crew has one row.
    #[serde(default)]
    model: Option<String>,
    /// The row's recorded effort selection; `/detail` only, optional when
    /// the crew and model name one row.
    #[serde(default)]
    effort: Option<String>,
    /// One tier the implementer size curves narrow to; `/size-curves` only,
    /// every tier when omitted.
    #[serde(default)]
    complexity: Option<String>,
    /// Which implementer tasks count by derived execution: `managed`
    /// (default), `all` or `interactive`.
    #[serde(default)]
    execution: Option<String>,
    /// Exclusive window end: an RFC 3339 instant or a duration ago such as
    /// `30d`. Omitted, the window ends now.
    #[serde(default)]
    until: Option<String>,
}

pub(super) async fn agent_scorecard(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(role): Path<String>,
    Query(query): Query<AgentScorecardQuery>,
) -> Response {
    let (role, selection) =
        match AgentRole::parse(&role).and_then(|role| Ok((role, selection(&query)?))) {
            Ok(selected) => selected,
            Err(error) => return error_response(error),
        };
    match facts(&state, runtime).await {
        Ok(served) => (
            served.age_header(),
            Json(agents::agent_scorecard(
                &served.body,
                role,
                &selection,
                AGENT_SCAN_TTL,
            )),
        )
            .into_response(),
        Err(error) => error_response(error),
    }
}

/// The implementer tier/type matrix, sliced from the same cached scan.
pub(super) async fn agent_by_type(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(query): Query<AgentScorecardQuery>,
) -> Response {
    let selection = match selection(&query) {
        Ok(selection) => selection,
        Err(error) => return error_response(error),
    };
    let types = runtime.task_types().to_vec();
    match facts(&state, runtime).await {
        Ok(served) => (
            served.age_header(),
            Json(agents::agent_by_type_scorecard(
                &served.body,
                &selection,
                &types,
                AGENT_SCAN_TTL,
            )),
        )
            .into_response(),
        Err(error) => error_response(error),
    }
}

/// The implementer regression rate by change size, sliced from the same
/// cached scan.
pub(super) async fn agent_size_curves(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(query): Query<AgentScorecardQuery>,
) -> Response {
    let selection = match selection(&query) {
        Ok(selection) => selection,
        Err(error) => return error_response(error),
    };
    let served = match facts(&state, runtime).await {
        Ok(served) => served,
        Err(error) => return error_response(error),
    };
    match agents::agent_size_curves(
        &served.body,
        &selection,
        query.complexity.as_deref(),
        AGENT_SCAN_TTL,
    ) {
        Ok(value) => (served.age_header(), Json(value)).into_response(),
        Err(error) => error_response(error),
    }
}

pub(super) async fn agent_detail(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(role): Path<String>,
    Query(query): Query<AgentScorecardQuery>,
) -> Response {
    let (role, selection) =
        match AgentRole::parse(&role).and_then(|role| Ok((role, selection(&query)?))) {
            Ok(selected) => selected,
            Err(error) => return error_response(error),
        };
    let Some(crew) = query.crew.as_deref().filter(|crew| !crew.trim().is_empty()) else {
        return bad_request("crew is required".into());
    };
    let served = match facts(&state, runtime).await {
        Ok(served) => served,
        Err(error) => return error_response(error),
    };
    match agents::agent_detail_with_effort(
        &served.body,
        role,
        &selection,
        crew,
        query.model.as_deref(),
        query.effort.as_deref(),
        AGENT_SCAN_TTL,
    ) {
        Ok(value) => (served.age_header(), Json(value)).into_response(),
        Err(error) => error_response(error),
    }
}

pub(super) async fn agent_attribution(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(query): Query<AgentScorecardQuery>,
) -> Response {
    let selection = match selection(&query) {
        Ok(selection) => selection,
        Err(error) => return error_response(error),
    };
    match facts(&state, runtime).await {
        Ok(served) => (
            served.age_header(),
            Json(agents::agent_attribution(
                &served.body,
                &selection,
                AGENT_SCAN_TTL,
            )),
        )
            .into_response(),
        Err(error) => error_response(error),
    }
}

/// Query string for the model ranking.
#[derive(Debug, Default, Deserialize)]
pub(super) struct AgentRankQuery {
    /// A time window: `1h`, `24h`, `7d`, `30d` (default), `90d` or `all`.
    #[serde(default)]
    window: Option<String>,
    /// The end before the settle lag: an RFC 3339 instant or a duration ago
    /// such as `7d`. Omitted, now.
    #[serde(default)]
    until: Option<String>,
    /// How long before `until` the ranked window ends (default `14d`).
    #[serde(default)]
    settle_lag: Option<String>,
    /// Which implementer tasks count by derived execution: `managed`
    /// (default), `all` or `interactive`.
    #[serde(default)]
    execution: Option<String>,
    /// `true` ranks the window ending at `until`, provisional.
    #[serde(default)]
    live: bool,
}

/// The implementer model ranking, sliced from the same cached scan with
/// the workspace's current season of noise scales.
pub(super) async fn agent_rank(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(query): Query<AgentRankQuery>,
) -> Response {
    let selection = match RankQuery::parse(
        query.window.as_deref(),
        query.until.as_deref(),
        query.settle_lag.as_deref(),
        query.execution.as_deref(),
        query.live,
    ) {
        Ok(selection) => selection,
        Err(error) => return error_response(error),
    };
    let seasons = match runtime.agent_rank_seasons() {
        Ok(seasons) => seasons,
        Err(error) => return error_response(error),
    };
    match facts(&state, runtime).await {
        Ok(served) => (
            served.age_header(),
            Json(agents::agent_rank(
                &served.body,
                &selection,
                &seasons,
                AGENT_SCAN_TTL,
            )),
        )
            .into_response(),
        Err(error) => error_response(error),
    }
}

/// Query string for `/api/agents/ci-failures`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct CiFailureSelection {
    /// `all` or a duration such as `14d` (default `30d`).
    #[serde(default)]
    window: Option<String>,
    /// Exclusive window end: an RFC 3339 instant or a duration ago.
    #[serde(default)]
    until: Option<String>,
    /// Only this task's failures.
    #[serde(default)]
    task: Option<String>,
    /// `code`, `flaky`, `infrastructure` or `unknown`.
    #[serde(default)]
    class: Option<String>,
}

/// The recorded required-check failures the selection names, newest first:
/// the payload `orbit agents ci-failures --json` prints.
pub(super) async fn agent_ci_failures(
    Ws(runtime): Ws,
    Query(query): Query<CiFailureSelection>,
) -> Response {
    let selection = match CiFailureQuery::parse(
        query.window.as_deref(),
        query.until.as_deref(),
        query.task.as_deref(),
        query.class.as_deref(),
    ) {
        Ok(selection) => selection,
        Err(error) => return error_response(error),
    };
    match blocking("CI failure records", move || {
        runtime.ci_failure_records(&selection)
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

fn selection(query: &AgentScorecardQuery) -> Result<AgentQuery, OrbitError> {
    AgentQuery::parse(query.window.as_deref(), query.min_support)?
        .with_execution(query.execution.as_deref())?
        .with_until(query.until.as_deref())
}

/// A bad selection is the caller's 400; anything else is a runtime error.
fn error_response(error: OrbitError) -> Response {
    match error {
        OrbitError::InvalidInput(message) => bad_request(message),
        error => map_runtime_error(error),
    }
}

async fn facts(
    state: &DashboardState,
    runtime: Arc<OrbitRuntime>,
) -> Result<Served<AgentFacts>, OrbitError> {
    let scan_runtime = Arc::clone(&runtime);
    state
        .agent_memo()
        .get_or_refresh(
            &runtime,
            (),
            AGENT_SCAN_TTL,
            AGENT_SCAN_STALE_LIMIT,
            move || scan_runtime.agent_scan(),
        )
        .await
}
