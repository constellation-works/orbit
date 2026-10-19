//! `GET /api/agents/{role}/scorecard`, `/api/agents/{role}/detail` and
//! `/api/agents/attribution`: the agent scorecards behind Observe › Agents.
//!
//! The scan, attribution and scores are the core agent scorecard service.
//! One scan per workspace runtime is cached for [`AGENT_SCAN_TTL`]; the role,
//! window, minimum support and detail only slice the cached facts, so
//! changing them or polling inside the cache period never rescans task
//! bundles. A stale scan is served while one background refresh replaces it.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::application::agents::{self, AgentFacts, AgentQuery, AgentRole};
use orbit_core::{OrbitError, OrbitRuntime};
use serde::Deserialize;

use super::{bad_request, map_runtime_error};
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
    match agents::agent_detail(
        &served.body,
        role,
        &selection,
        crew,
        query.model.as_deref(),
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

fn selection(query: &AgentScorecardQuery) -> Result<AgentQuery, OrbitError> {
    AgentQuery::parse(query.window.as_deref(), query.min_support)
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
