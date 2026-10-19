//! `GET /api/codebase/analytics` and `/related`: which repository files are expensive
//! or risky to change and which files change alongside them.
//!
//! The scan and rankings are the core codebase analytics service, shared with
//! `orbit codebase` and the `orbit.codebase.*` tools. One scan per workspace
//! runtime is cached for [`CODEBASE_ANALYTICS_TTL`]; the window, minimum
//! support, rollup and regression filters (`nature`, `escape`, `layer`,
//! `detected_by`; an unknown value is a 400) only slice the cached facts, so
//! changing them or polling inside the cache period never rescans task
//! bundles. Analytics uses history receipts only; related-file scans have a
//! separate cache entry per window so delivery fallback never observes tasks
//! outside the requested window.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::application::codebase::{self, CodebaseQuery, RegressionFilter};
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
    /// Regression filters: a nature, escape, fix layer or detector.
    #[serde(default)]
    nature: Option<String>,
    #[serde(default)]
    escape: Option<String>,
    #[serde(default)]
    layer: Option<String>,
    #[serde(default)]
    detected_by: Option<String>,
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
    let selection = CodebaseQuery::parse(
        query.window.as_deref(),
        query.min_support,
        query.rollup.as_deref(),
    )
    .and_then(|selection| {
        Ok(selection.with_regression_filter(RegressionFilter::parse(
            query.nature.as_deref(),
            query.escape.as_deref(),
            query.layer.as_deref(),
            query.detected_by.as_deref(),
        )?))
    });
    let query = match selection {
        Ok(query) => query,
        Err(orbit_core::OrbitError::InvalidInput(message)) => return bad_request(message),
        Err(error) => return map_runtime_error(error),
    };

    let scan_runtime = Arc::clone(&runtime);
    let facts = match state
        .codebase_memo()
        .get_or_compute(&runtime, None, CODEBASE_ANALYTICS_TTL, move || {
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
    let window = query.window.unwrap_or_else(|| "30d".into());
    let facts = match state
        .codebase_memo()
        .get_or_compute(&runtime, Some(window), CODEBASE_ANALYTICS_TTL, move || {
            scan_runtime.codebase_related_scan(&selection)
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
