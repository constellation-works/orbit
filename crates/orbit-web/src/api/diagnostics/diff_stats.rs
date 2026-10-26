//! Lines added and deleted per landed delivery over time, by line class and
//! by implementing crew, for Analytics › Metrics [ORB-18383].
//!
//! Each landed delivery counts once, in the bucket of its landing time. A
//! complexity filter stratifies the crew breakdown: crews draw from
//! complexity pools, so an unfiltered per-crew total mostly measures task
//! difficulty. Every cell carries totals, per-delivery means and medians,
//! so the chart can switch between totals and a per-task view without a
//! refetch.

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::{LandedDiffStats, OrbitRuntime};
use orbit_types::task::{UNSET_BUCKET, complexity_bucket, complexity_bucket_ord};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::super::{DiagnosticsQuery, map_runtime_error};
use super::metrics::DiagnosticsRange;
use super::trend::{TrendWindow, trend_range};
use crate::state::Ws;

/// Complexities a request may name; `unassessed` is an alias of the unset
/// bucket.
const COMPLEXITIES: [&str; 5] = ["low", "medium", "hard", "xhard", UNSET_BUCKET];
/// A point summarizing fewer deliveries than this is marked thin.
const MIN_DELIVERIES: usize = 3;
/// Crews drawn as their own series; the rest fold into `other`.
const MAX_CREWS: usize = 7;
/// The per-delivery measures every cell summarizes. `net` is code and tests
/// added minus deleted; other lines (docs, config, assets) stay out of it.
const MEASURES: [&str; 7] = [
    "code_added",
    "code_deleted",
    "tests_added",
    "tests_deleted",
    "other_added",
    "other_deleted",
    "net",
];
/// The series key of a delivery whose crew was not recorded.
const UNKNOWN_CREW: &str = "unknown";

#[derive(Deserialize, Default)]
pub(in crate::api) struct DiffStatsQuery {
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    month: Option<String>,
    /// Comma-separated complexities to count; every one when absent.
    #[serde(default)]
    complexity: Option<String>,
}

fn parse_complexities(raw: Option<&str>) -> Result<Option<BTreeSet<&'static str>>, String> {
    let requested: Vec<&str> = raw
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect();
    if requested.is_empty() {
        return Ok(None);
    }
    let mut set = BTreeSet::new();
    for value in requested {
        let bucket = if value == "unassessed" {
            UNSET_BUCKET
        } else {
            value
        };
        let Some(known) = COMPLEXITIES.iter().find(|known| **known == bucket) else {
            return Err(format!(
                "unknown complexity '{value}': expected low, medium, hard, xhard or unassessed"
            ));
        };
        set.insert(*known);
    }
    Ok(Some(set))
}

pub(in crate::api) async fn diagnostics_diff_stats(
    Ws(runtime): Ws,
    Query(q): Query<DiffStatsQuery>,
) -> Response {
    let complexities = match parse_complexities(q.complexity.as_deref()) {
        Ok(complexities) => complexities,
        Err(message) => {
            return map_runtime_error(orbit_core::OrbitError::InvalidInput(message));
        }
    };
    let range = match trend_range(&DiagnosticsQuery {
        outcome: None,
        since: q.since,
        month: q.month,
        limit: None,
    }) {
        Ok(range) => range,
        Err(error) => return map_runtime_error(error),
    };
    match super::super::blocking("diff stats trend", move || {
        diff_stats_trend(&runtime, &range, complexities.as_ref())
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

/// One delivery's measures, in [`MEASURES`] order.
fn measures(row: &LandedDiffStats) -> [i64; 7] {
    let n = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
    let (code, tests, other) = (&row.code, &row.tests, &row.other);
    [
        n(code.added),
        n(code.deleted),
        n(tests.added),
        n(tests.deleted),
        n(other.added),
        n(other.deleted),
        n(code.added) + n(tests.added) - n(code.deleted) - n(tests.deleted),
    ]
}

/// The deliveries one point (or a series' window total) summarizes.
#[derive(Clone, Default)]
struct Cell {
    rows: Vec<[i64; 7]>,
}

impl Cell {
    fn to_json(&self) -> Value {
        let n = self.rows.len();
        let mut sum = Map::new();
        let mut mean = Map::new();
        let mut median = Map::new();
        for (index, measure) in MEASURES.iter().enumerate() {
            let mut values: Vec<i64> = self.rows.iter().map(|row| row[index]).collect();
            values.sort_unstable();
            let total: i64 = values.iter().sum();
            sum.insert((*measure).to_string(), json!(total));
            if n == 0 {
                mean.insert((*measure).to_string(), Value::Null);
                median.insert((*measure).to_string(), Value::Null);
                continue;
            }
            let middle = if n % 2 == 1 {
                values[n / 2] as f64
            } else {
                (values[n / 2 - 1] as f64 + values[n / 2] as f64) / 2.0
            };
            mean.insert((*measure).to_string(), json!(total as f64 / n as f64));
            median.insert((*measure).to_string(), json!(middle));
        }
        json!({
            "n": n,
            "thin": n > 0 && n < MIN_DELIVERIES,
            "sum": sum,
            "mean": mean,
            "median": median,
        })
    }
}

struct Series {
    points: Vec<Cell>,
    total: Cell,
}

impl Series {
    fn new(buckets: usize) -> Self {
        Self {
            points: vec![Cell::default(); buckets],
            total: Cell::default(),
        }
    }

    fn record(&mut self, index: usize, row: [i64; 7]) {
        self.points[index].rows.push(row);
        self.total.rows.push(row);
    }

    fn to_json(&self, key: &str, crews: &[String]) -> Value {
        json!({
            "key": key,
            "crews": crews,
            "points": self.points.iter().map(Cell::to_json).collect::<Vec<_>>(),
            "total": self.total.to_json(),
        })
    }
}

fn diff_stats_trend(
    runtime: &OrbitRuntime,
    range: &DiagnosticsRange,
    complexities: Option<&BTreeSet<&'static str>>,
) -> Result<Value, orbit_core::OrbitError> {
    let rows = runtime.landed_diff_stats(TrendWindow::read_since(range))?;
    let window = TrendWindow::new(range, rows.first().map(|row| row.landed_at));
    let bucket_count = window.bucket_count();
    let mut complexity_counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut in_window = Vec::new();
    for row in &rows {
        let Some(index) = window.bucket_of(row.landed_at) else {
            continue;
        };
        let complexity = complexity_bucket(row.complexity.as_deref());
        *complexity_counts.entry(complexity).or_default() += 1;
        if complexities.is_none_or(|selected| selected.contains(complexity)) {
            in_window.push((index, row));
        }
    }

    // The busiest crews get their own series; ties break by name.
    let mut crew_counts: BTreeMap<&str, u64> = BTreeMap::new();
    for (_, row) in &in_window {
        *crew_counts
            .entry(row.crew.as_deref().unwrap_or(UNKNOWN_CREW))
            .or_default() += 1;
    }
    let mut ranked: Vec<(&str, u64)> = crew_counts.iter().map(|(k, v)| (*k, *v)).collect();
    ranked.sort_by(|(left, left_n), (right, right_n)| right_n.cmp(left_n).then(left.cmp(right)));
    let drawn: Vec<&str> = ranked
        .iter()
        .take(MAX_CREWS)
        .map(|(crew, _)| *crew)
        .collect();
    let folded: Vec<String> = ranked
        .iter()
        .skip(MAX_CREWS)
        .map(|(crew, _)| (*crew).to_string())
        .collect();

    let mut overall = Series::new(bucket_count);
    let mut by_crew: BTreeMap<&str, Series> = drawn
        .iter()
        .map(|crew| (*crew, Series::new(bucket_count)))
        .collect();
    let mut other = Series::new(bucket_count);
    for (index, row) in &in_window {
        let values = measures(row);
        overall.record(*index, values);
        let crew = row.crew.as_deref().unwrap_or(UNKNOWN_CREW);
        by_crew
            .get_mut(crew)
            .unwrap_or(&mut other)
            .record(*index, values);
    }
    let mut crews: Vec<Value> = drawn
        .iter()
        .filter_map(|crew| {
            by_crew
                .get(crew)
                .map(|series| series.to_json(crew, &[(*crew).to_string()]))
        })
        .collect();
    if !folded.is_empty() {
        crews.push(other.to_json("other", &folded));
    }

    let mut available: Vec<(&str, u64)> = complexity_counts.into_iter().collect();
    available.sort_by_key(|(complexity, _)| complexity_bucket_ord(complexity));

    Ok(json!({
        "window": window.to_json(),
        "complexity": complexities.map(|set| set.iter().collect::<Vec<_>>()),
        "min_deliveries": MIN_DELIVERIES,
        "measures": MEASURES,
        "bucket_starts": (0..bucket_count)
            .map(|index| window.bucket_start(index).to_rfc3339())
            .collect::<Vec<_>>(),
        "overall": overall.to_json("all", &[]),
        "crews": crews,
        "available_complexities": available
            .into_iter()
            .map(|(complexity, deliveries)| json!({ "complexity": complexity, "deliveries": deliveries }))
            .collect::<Vec<_>>(),
    }))
}
