//! Defects filed per bucket, for Analytics › Metrics.
//!
//! A defect is a `bug` task, counted at its `created_at`. Each one is
//! classified twice: by lineage (what its `regression_from` relations name as
//! the cause) and by origin (the provenance tag of whatever filed it).

use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::OrbitRuntime;
use orbit_types::task::TaskType;
use serde_json::{Map, Value, json};

use super::super::{DiagnosticsQuery, map_runtime_error};
use super::metrics::DiagnosticsRange;
use super::trend::{TrendWindow, trend_range};
use crate::state::Ws;

/// Lineage series, bottom of the stack first.
const LINEAGES: [&str; 3] = ["fix_for_fix", "regression", "unattributed"];

/// Origin series in display order. `other` covers defects no finder tagged:
/// filed by hand, or by a source this list does not name.
const ORIGINS: [&str; 11] = [
    "code-review",
    "full-code-review",
    "delivery-code-review",
    "qa-sweep",
    "qa-full-sweep",
    "security-review",
    "ci-failure-sweep",
    "scanning-sweep",
    "review-reconciliation",
    "friction-curation",
    "other",
];

pub(in crate::api) async fn diagnostics_defects(
    Ws(runtime): Ws,
    Query(q): Query<DiagnosticsQuery>,
) -> Response {
    let range = match trend_range(&q) {
        Ok(range) => range,
        Err(error) => return map_runtime_error(error),
    };
    match super::super::blocking("defect trend", move || defect_trend(&runtime, &range)).await {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

/// What a defect's `regression_from` relations say caused it.
fn lineage(regression_from_types: &[Option<TaskType>]) -> &'static str {
    if regression_from_types.is_empty() {
        "unattributed"
    } else if regression_from_types.contains(&Some(TaskType::Bug)) {
        // The cause was itself a fix.
        "fix_for_fix"
    } else {
        "regression"
    }
}

/// The finder that filed a defect, from its provenance tags. The first
/// matching rule wins, so a full code review's findings, which also carry
/// `code-review`, count as `full-code-review`.
fn origin(tags: &[String]) -> &'static str {
    let has = |tag: &str| tags.iter().any(|candidate| candidate == tag);
    let has_prefix = |prefix: &str| tags.iter().any(|candidate| candidate.starts_with(prefix));
    if has("full-code-review") {
        "full-code-review"
    } else if has("delivery-code-review") {
        "delivery-code-review"
    } else if has("code-review") {
        "code-review"
    } else if has("qa-full-sweep") {
        "qa-full-sweep"
    } else if has("qa-sweep") || has_prefix("qa-sweep-") {
        "qa-sweep"
    } else if has("security-review") {
        "security-review"
    } else if has("ci-failure-sweep") || has_prefix("ci-failure:") {
        "ci-failure-sweep"
    } else if has("dependabot-sweep") || has("code-scanning-sweep") || has("secret-scanning-sweep")
    {
        "scanning-sweep"
    } else if has_prefix("review-reconciliation:") {
        "review-reconciliation"
    } else if has("friction-curation") {
        "friction-curation"
    } else {
        "other"
    }
}

#[derive(Clone)]
struct Counts {
    total: u64,
    lineage: [u64; LINEAGES.len()],
    origin: [u64; ORIGINS.len()],
}

impl Default for Counts {
    fn default() -> Self {
        Self {
            total: 0,
            lineage: [0; LINEAGES.len()],
            origin: [0; ORIGINS.len()],
        }
    }
}

impl Counts {
    fn record(&mut self, regression_from_types: &[Option<TaskType>], tags: &[String]) {
        self.total += 1;
        let lineage = lineage(regression_from_types);
        let origin = origin(tags);
        if let Some(index) = LINEAGES.iter().position(|key| *key == lineage) {
            self.lineage[index] += 1;
        }
        if let Some(index) = ORIGINS.iter().position(|key| *key == origin) {
            self.origin[index] += 1;
        }
    }

    fn to_json(&self) -> Value {
        let series = |keys: &[&str], counts: &[u64]| {
            keys.iter()
                .zip(counts)
                .map(|(key, count)| ((*key).to_string(), json!(count)))
                .collect::<Map<_, _>>()
        };
        json!({
            "total": self.total,
            "lineage": series(&LINEAGES, &self.lineage),
            "origin": series(&ORIGINS, &self.origin),
        })
    }
}

fn defect_trend(
    runtime: &OrbitRuntime,
    range: &DiagnosticsRange,
) -> Result<Value, orbit_core::OrbitError> {
    let filings = runtime.task_defect_filings(TrendWindow::read_since(range))?;
    let window = TrendWindow::new(range, filings.first().map(|filing| filing.filed_at));
    let mut buckets = vec![Counts::default(); window.bucket_count()];
    let mut current = Counts::default();
    let mut previous = Counts::default();
    for filing in &filings {
        if let Some(index) = window.bucket_of(filing.filed_at) {
            buckets[index].record(&filing.regression_from_types, &filing.tags);
            current.record(&filing.regression_from_types, &filing.tags);
        } else if window.in_previous(filing.filed_at) {
            previous.record(&filing.regression_from_types, &filing.tags);
        }
    }
    let buckets: Vec<Value> = buckets
        .iter()
        .enumerate()
        .map(|(index, counts)| {
            let mut row = counts.to_json();
            row["start"] = json!(window.bucket_start(index).to_rfc3339());
            row
        })
        .collect();
    Ok(json!({
        "window": window.to_json(),
        "lineages": LINEAGES,
        "origins": ORIGINS,
        "buckets": buckets,
        "current": current.to_json(),
        "previous": window.previous.map(|_| previous.to_json()),
    }))
}
