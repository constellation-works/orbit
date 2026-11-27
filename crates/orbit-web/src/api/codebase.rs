//! `GET /api/codebase/analytics`, `/show` and `/related`: which repository
//! files are expensive or risky to change, one path's standing with the tasks
//! and regression sites behind it, and which files change alongside them.
//!
//! The scan and rankings are the core codebase analytics service, shared with
//! `orbit codebase` and the `orbit.codebase.*` tools. The scan reuses the
//! store's derived per-task facts and rereads only the bundles of tasks that
//! changed since, so even this server's first scan is cold only once per
//! workspace. One scan per workspace runtime is cached for
//! [`CODEBASE_ANALYTICS_TTL`]; the window, minimum support, rollup and
//! regression filters (`nature`, `escape`, `layer`, `detected_by`; an
//! unknown value is a 400) only slice the cached facts, so changing them or
//! polling inside the cache period never rescans. Show reads the same cached
//! facts and selection plus a required `path`. Past that period, for up
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
use orbit_core::app::codebase::{self as codebase_app, Command, Outcome};
use orbit_core::application::codebase::{self, CodebaseQuery, RegressionFilter};
use serde::Deserialize;

use super::{bad_request, blocking_join, map_runtime_error};
#[cfg(doc)]
use crate::runtime_memo::CODEBASE_ANALYTICS_STALE_LIMIT;
use crate::runtime_memo::CODEBASE_ANALYTICS_TTL;
use crate::state::{CodebaseWs, DashboardState};

/// Query string for `GET /api/codebase/analytics` and `/show`.
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
    /// The ranked key `/show` reports; required there. Analytics does not
    /// read it but still rejects a non-string value.
    #[serde(default)]
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

/// The validated selection of an analytics or show query; a layer is checked
/// against the workspace's `workflow.regression_layers`.
fn codebase_selection(
    core: &orbit_core::Core,
    query: &CodebaseAnalyticsQuery,
) -> Result<CodebaseQuery, Box<Response>> {
    let layers = || match codebase_app::exec(core, Command::RegressionLayers)? {
        Outcome::RegressionLayers(layers) => Ok(layers),
        Outcome::Value(_) | Outcome::Facts(_) => Err(mismatched("regression layers")),
    };
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
            &layers()?,
        )?))
    });
    selection.map_err(|error| {
        Box::new(match error {
            orbit_core::OrbitError::InvalidInput(message) => bad_request(message),
            error => map_runtime_error(error),
        })
    })
}

/// The workspace's history-only scan, cached and served stale like the
/// analytics payload, with its `Age` header.
async fn cached_codebase_scan(
    state: &DashboardState,
    ws: &CodebaseWs,
) -> Result<crate::runtime_memo::Served<codebase::CodebaseFacts>, Box<Response>> {
    let core = Arc::clone(&ws.core);
    ws.cached_scan(state, move || facts(&core, Command::Scan))
        .await
        .map_err(|error| Box::new(map_runtime_error(error)))
}

/// The codebase app's answer to a read, which is always the model it was
/// asked for.
fn mismatched(read: &str) -> orbit_core::OrbitError {
    orbit_core::OrbitError::Execution(format!(
        "the codebase {read} read answered with another model"
    ))
}

/// The scan a scan read answers with.
fn facts(
    core: &orbit_core::Core,
    command: Command,
) -> Result<codebase::CodebaseFacts, orbit_core::OrbitError> {
    match codebase_app::exec(core, command)? {
        Outcome::Facts(facts) => Ok(facts),
        Outcome::Value(_) | Outcome::RegressionLayers(_) => Err(mismatched("scan")),
    }
}

pub(super) async fn codebase_analytics(
    State(state): State<DashboardState>,
    ws: CodebaseWs,
    Query(query): Query<CodebaseAnalyticsQuery>,
) -> Response {
    let query = match codebase_selection(&ws.core, &query) {
        Ok(query) => query,
        Err(response) => return *response,
    };
    let served = match cached_codebase_scan(&state, &ws).await {
        Ok(served) => served,
        Err(response) => return *response,
    };
    // File rollup marks each ranked path with checkout existence at query
    // time (`canonicalize`), which is blocking filesystem I/O. The scan
    // itself is already off the executor via the memo; run the slicing and
    // its checkout lookups on the blocking pool as well so a slow checkout
    // never parks the async worker serving the request, on a miss or a hit.
    let age = served.age_header();
    let facts = Arc::clone(&served.body);
    let payload = match blocking_join("codebase analytics", move || {
        codebase::codebase_analytics(&facts, &query, CODEBASE_ANALYTICS_TTL)
    })
    .await
    {
        Ok(payload) => payload,
        Err(response) => return *response,
    };
    (age, Json(payload)).into_response()
}

/// One path's standing in every ranking, as `orbit codebase show` reports
/// it; a path nothing in the window touched or sited is a 400.
pub(super) async fn codebase_show(
    State(state): State<DashboardState>,
    ws: CodebaseWs,
    Query(query): Query<CodebaseAnalyticsQuery>,
) -> Response {
    let selection = match codebase_selection(&ws.core, &query) {
        Ok(selection) => selection,
        Err(response) => return *response,
    };
    let Some(path) = query.path.as_deref().filter(|path| !path.trim().is_empty()) else {
        return bad_request("path is required".into());
    };
    let served = match cached_codebase_scan(&state, &ws).await {
        Ok(served) => served,
        Err(response) => return *response,
    };
    // Same blocking boundary as analytics: the detail's checkout lookup must
    // not run on the request worker, including when the scan is a memo hit.
    // `blocking_join` preserves the typed error so the 400/500 mapping below
    // is unchanged; only a join panic becomes a 500.
    let age = served.age_header();
    let facts = Arc::clone(&served.body);
    let path = path.to_owned();
    let detail = match blocking_join("codebase path detail", move || {
        codebase::codebase_path_detail(&facts, &selection, &path)
    })
    .await
    {
        Ok(detail) => detail,
        Err(response) => return *response,
    };
    match detail {
        Ok(value) => (age, Json(value)).into_response(),
        Err(orbit_core::OrbitError::InvalidInput(message)) => bad_request(message),
        Err(error) => map_runtime_error(error),
    }
}

pub(super) async fn codebase_related(
    State(state): State<DashboardState>,
    ws: CodebaseWs,
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
    let core = Arc::clone(&ws.core);
    let scan_query = selection.codebase_query();
    let facts = match ws
        .cached_related_scan(&state, selection.window().to_owned(), move || {
            facts(&core, Command::RelatedScan(scan_query))
        })
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
