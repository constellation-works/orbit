//! `GET /api/codebase/analytics` and `/related`: which repository files are expensive
//! or risky to change and which files change alongside them.
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
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    top: Option<usize>,
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

pub(super) async fn codebase_related(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(query): Query<CodebaseAnalyticsQuery>,
) -> Response {
    let selection = match CodebaseQuery::parse(
        Some(query.window.as_deref().unwrap_or("30d")),
        query.min_support,
        Some("file"),
    ) {
        Ok(query) => query,
        Err(error) => return map_runtime_error(error),
    };
    let Some(path) = query.path else {
        return bad_request("path is required".into());
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
    match codebase::codebase_related(&facts, &selection, &path, query.top.unwrap_or(10)) {
        Ok(value) => Json(value).into_response(),
        Err(error) => map_runtime_error(error),
    }
}
