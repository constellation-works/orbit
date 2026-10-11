//! `GET /api/codebase/analytics`: which parts of the repository are expensive
//! or risky for agents to change, from records Orbit already keeps.
//!
//! "Touched" means a task's declared footprint (`context_files`, widened
//! selectors included), not the diff that landed. One scan per workspace
//! runtime is cached for [`CODEBASE_ANALYTICS_TTL`]; the window, minimum
//! support and rollup only slice the cached facts, so changing them or
//! polling inside the cache period never rescans task bundles.

mod facts;
mod sections;

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use chrono::{Duration, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::json;

use super::{bad_request, map_runtime_error};
use crate::runtime_memo::CODEBASE_ANALYTICS_TTL;
use crate::state::{DashboardState, Ws};

pub(crate) use facts::CodebaseFacts;
use sections::{Params, Rollup};

const DEFAULT_WINDOW: &str = "90d";
const DEFAULT_MIN_SUPPORT: usize = 15;
const WINDOWS: &[(&str, Option<i64>)] = &[
    ("7d", Some(7)),
    ("30d", Some(30)),
    ("90d", Some(90)),
    ("all", None),
];

/// Query string for `GET /api/codebase/analytics`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct CodebaseQuery {
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
    Query(query): Query<CodebaseQuery>,
) -> Response {
    let window = query.window.as_deref().unwrap_or(DEFAULT_WINDOW);
    let Some(&(window, days)) = WINDOWS.iter().find(|(name, _)| *name == window) else {
        return bad_request(format!(
            "window must be one of 7d, 30d, 90d or all; got '{window}'"
        ));
    };
    let min_support = query.min_support.unwrap_or(DEFAULT_MIN_SUPPORT);
    if min_support == 0 {
        return bad_request("min_support must be at least 1".to_string());
    }
    let rollup_name = query.rollup.as_deref().unwrap_or("file");
    let Some(rollup) = Rollup::parse(rollup_name) else {
        return bad_request(format!(
            "rollup must be one of file, dir or crate; got '{rollup_name}'"
        ));
    };

    let scan_runtime = Arc::clone(&runtime);
    let facts = match state
        .codebase_memo()
        .get_or_compute(&runtime, (), CODEBASE_ANALYTICS_TTL, move || {
            facts::scan(&scan_runtime)
        })
        .await
    {
        Ok(facts) => facts,
        Err(error) => return map_runtime_error(error),
    };

    let since = days.map(|days| Utc::now() - Duration::days(days));
    let params = Params {
        since,
        min_support,
        rollup,
    };
    let mut payload = sections::all_sections(&facts, &params);
    if let Some(object) = payload.as_object_mut() {
        object.extend(
            json!({
                "window": window,
                "since": since.map(|since| since.to_rfc3339_opts(SecondsFormat::Secs, true)),
                "min_support": min_support,
                "rollup": rollup.as_str(),
                "touched": "declared_footprint",
                "computed_at": facts.computed_at.to_rfc3339_opts(SecondsFormat::Millis, true),
                "cache_ttl_seconds": CODEBASE_ANALYTICS_TTL.as_secs(),
                "tasks_scanned": facts.tasks.len(),
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        );
    }
    Json(payload).into_response()
}
