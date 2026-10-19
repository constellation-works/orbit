//! `GET /api/orchestrators/scorecard` and `/detail`: the orchestrator
//! scorecard behind Analytics › Orchestrators, separate from the
//! `/api/metrics/orchestrators` activity counts and the Cost view.
//!
//! The scan and rows are the core orchestrator scorecard service, shared with
//! `orbit orchestrator` and the `orbit.orchestrator.*` tools. One scan per
//! workspace runtime is cached for [`ORCHESTRATOR_SCORECARD_TTL`]; the window
//! and minimum support only slice the cached facts, so changing them, opening
//! a drill-down or polling inside the cache period never rescans task bundles.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::application::orchestrators::{self, OrchestratorFacts, OrchestratorQuery};
use orbit_core::{OrbitError, OrbitRuntime};
use serde::Deserialize;

use super::{bad_request, map_runtime_error};
use crate::runtime_memo::ORCHESTRATOR_SCORECARD_TTL;
use crate::state::{DashboardState, Ws};

/// Query string for both orchestrator scorecard routes.
#[derive(Debug, Default, Deserialize)]
pub(super) struct OrchestratorScorecardQuery {
    /// `1h`, `24h`, `7d`, `30d` (default), `90d` or `all`.
    #[serde(default)]
    window: Option<String>,
    /// Owned tasks a row needs before its rates are comparable (default 10,
    /// at least 1).
    #[serde(default)]
    min_support: Option<usize>,
    /// The orchestrator `/detail` reports; required there.
    #[serde(default)]
    orchestrator: Option<String>,
}

pub(super) async fn orchestrator_scorecard(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(query): Query<OrchestratorScorecardQuery>,
) -> Response {
    let selection = match OrchestratorQuery::parse(query.window.as_deref(), query.min_support) {
        Ok(selection) => selection,
        Err(error) => return error_response(error),
    };
    match facts(&state, runtime).await {
        Ok(facts) => Json(orchestrators::orchestrator_scorecard(
            &facts,
            &selection,
            ORCHESTRATOR_SCORECARD_TTL,
        ))
        .into_response(),
        Err(error) => error_response(error),
    }
}

pub(super) async fn orchestrator_detail(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(query): Query<OrchestratorScorecardQuery>,
) -> Response {
    let selection = match OrchestratorQuery::parse(query.window.as_deref(), query.min_support) {
        Ok(selection) => selection,
        Err(error) => return error_response(error),
    };
    let Some(name) = query.orchestrator.filter(|name| !name.trim().is_empty()) else {
        return bad_request("orchestrator is required".into());
    };
    let facts = match facts(&state, runtime).await {
        Ok(facts) => facts,
        Err(error) => return error_response(error),
    };
    match orchestrators::orchestrator_detail(&facts, &selection, &name) {
        Ok(value) => Json(value).into_response(),
        Err(error) => error_response(error),
    }
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
) -> Result<Arc<OrchestratorFacts>, OrbitError> {
    let scan_runtime = Arc::clone(&runtime);
    state
        .orchestrator_memo()
        .get_or_compute(&runtime, (), ORCHESTRATOR_SCORECARD_TTL, move || {
            scan_runtime.orchestrator_scan()
        })
        .await
        .map(|served| served.body)
}
