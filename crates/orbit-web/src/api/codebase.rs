//! `GET /api/codebase/analytics` and `/related`: which repository files are expensive
//! or risky to change and which files change alongside them.
//!
//! The scan and rankings are the core codebase analytics service, shared with
//! `orbit codebase` and the `orbit.codebase.*` tools. The scan reuses the
//! store's derived per-task facts and rereads only the bundles of tasks that
//! changed since, so even this server's first scan is cold only once per
//! workspace. One scan per workspace runtime is cached for
//! [`CODEBASE_ANALYTICS_TTL`]; the window, minimum support, rollup and
//! regression filters (`nature`, `escape`, `layer`, `detected_by`; an
//! unknown value is a 400) only slice the cached facts, so changing them or
//! polling inside the cache period never rescans. Past that period, for up
//! to [`CODEBASE_ANALYTICS_STALE_LIMIT`], a request is answered from the last
//! facts at once while one background scan replaces them: the payload's
//! `computed_at` and the `Age` header say how old they are. Analytics uses
//! history receipts only; related-file scans have a separate cache entry per
//! resolved window so delivery fallback never observes tasks outside it.
//! Related reads `path`, `window`, `min_support` and `top` only. Its omitted
//! window (30 days) and partner cap (10) come from the shared related
//! selection, which also keys that cache entry.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::application::codebase::{self, CodebaseQuery, RegressionFilter};
use serde::Deserialize;

use super::{bad_request, map_runtime_error};
use crate::runtime_memo::{CODEBASE_ANALYTICS_STALE_LIMIT, CODEBASE_ANALYTICS_TTL};
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
    /// Present so a non-string `path` is still rejected. Analytics does not read it.
    #[serde(default)]
    #[allow(dead_code)]
    path: Option<String>,
    /// Present so a non-numeric `top` is still rejected. Analytics does not read it.
    #[serde(default)]
    #[allow(dead_code)]
    top: Option<usize>,
}

/// Query string for `GET /api/codebase/related`.
///
/// Rollup and regression filters are not parameters. An omitted window or
/// `top` is resolved by [`orbit_core::application::codebase::RelatedQuery`].
#[derive(Debug, Default, Deserialize)]
pub(super) struct CodebaseRelatedQuery {
    /// `7d`, `30d`, `90d` or `all`.
    #[serde(default)]
    window: Option<String>,
    /// Smallest shared landed-task count a partner needs (at least 1).
    #[serde(default)]
    min_support: Option<usize>,
    /// Repository-relative file path. Required.
    #[serde(default)]
    path: Option<String>,
    /// Maximum partners, from 1 to 100.
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
    let served = match state
        .codebase_memo()
        .get_or_refresh(
            &runtime,
            None,
            CODEBASE_ANALYTICS_TTL,
            CODEBASE_ANALYTICS_STALE_LIMIT,
            move || scan_runtime.codebase_scan(),
        )
        .await
    {
        Ok(served) => served,
        Err(error) => return map_runtime_error(error),
    };
    (
        served.age_header(),
        Json(codebase::codebase_analytics(
            &served.body,
            &query,
            CODEBASE_ANALYTICS_TTL,
        )),
    )
        .into_response()
}

pub(super) async fn codebase_related(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(query): Query<CodebaseRelatedQuery>,
) -> Response {
    let selection = match codebase::RelatedQuery::parse(
        query.window.as_deref(),
        query.min_support,
        query.top,
    ) {
        Ok(selection) => selection,
        Err(orbit_core::OrbitError::InvalidInput(message)) => return bad_request(message),
        Err(error) => return map_runtime_error(error),
    };
    let Some(path) = query.path else {
        return bad_request("path is required".into());
    };
    let scan_runtime = Arc::clone(&runtime);
    let facts = match state
        .codebase_memo()
        .get_or_compute(
            &runtime,
            Some(selection.window().to_owned()),
            CODEBASE_ANALYTICS_TTL,
            move || {
                let query = selection.codebase_query();
                scan_runtime.codebase_related_scan(&query)
            },
        )
        .await
    {
        Ok(served) => served.body,
        Err(error) => return map_runtime_error(error),
    };
    match codebase::codebase_related(&facts, &selection.codebase_query(), &path, selection.top()) {
        Ok(value) => Json(value).into_response(),
        Err(orbit_core::OrbitError::InvalidInput(message)) => bad_request(message),
        Err(error) => map_runtime_error(error),
    }
}
