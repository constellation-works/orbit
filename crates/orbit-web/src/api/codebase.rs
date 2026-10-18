//! `GET /api/codebase/analytics`: which parts of the repository are expensive
//! or risky for agents to change, from records Orbit already keeps.
//!
//! The scan and rankings are the core codebase analytics service, shared with
//! `orbit codebase` and the `orbit.codebase.*` tools. One scan per workspace
//! runtime is cached for [`CODEBASE_ANALYTICS_TTL`]; the window, minimum
//! support and rollup only slice the cached facts, so changing them or
//! polling inside the cache period never rescans task bundles.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::application::codebase::{self, CodebaseQuery};
use serde::Deserialize;

use super::{bad_request, map_runtime_error};
use crate::runtime_memo::CODEBASE_ANALYTICS_TTL;
use crate::state::{DashboardState, Ws};

/// Query string for `GET /api/codebase/analytics`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct CodebaseAnalyticsQuery {
    /// `7d`, `30d`, `90d` (default) or `all`.
    #[serde(default)]
    window: Option<String>,
    /// Smallest denominator a ranked row needs (default 15, at least 1).
    #[serde(default)]
    min_support: Option<usize>,
    /// `file` (default), `dir` or `crate`.
    #[serde(default)]
    rollup: Option<String>,
}

pub(super) async fn codebase_analytics(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(query): Query<CodebaseAnalyticsQuery>,
) -> Response {
    let query = match CodebaseQuery::parse(
        query.window.as_deref(),
        query.min_support,
        query.rollup.as_deref(),
    ) {
        Ok(query) => query,
        Err(orbit_core::OrbitError::InvalidInput(message)) => return bad_request(message),
        Err(error) => return map_runtime_error(error),
    };

    let scan_runtime = Arc::clone(&runtime);
    let facts = match state
        .codebase_memo()
        .get_or_compute(&runtime, (), CODEBASE_ANALYTICS_TTL, move || {
            scan_runtime.codebase_scan()
        })
        .await
    {
        Ok(served) => served.body,
        Err(error) => return map_runtime_error(error),
    };
    Json(codebase::codebase_analytics(
        &facts,
        &query,
        CODEBASE_ANALYTICS_TTL,
    ))
    .into_response()
}
