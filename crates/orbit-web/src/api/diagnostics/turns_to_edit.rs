//! Tool calls and result bytes before the first file edit, by complexity
//! and tag cohort, for Analytics › Metrics.
//!
//! Each terminal task counts once, in the bucket of its index `updated_at`.
//! Its score is the first-edit score of its first implement invocation, the
//! same definition the experiment readout uses. A task whose first
//! invocation has no scoreable edit is excluded and counted, never averaged
//! as zero.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::OrbitRuntime;
use orbit_types::task::{TaskStatus, UNSET_BUCKET, complexity_bucket_ord};
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::{DiagnosticsQuery, map_runtime_error};
use super::metrics::DiagnosticsRange;
use super::trend::{TrendWindow, trend_range};
use crate::state::Ws;

/// Complexities a request may name; `unassessed` is an alias of the unset
/// bucket.
const COMPLEXITIES: [&str; 5] = ["low", "medium", "hard", "xhard", UNSET_BUCKET];
/// Series drawn even when the window holds none of their tasks.
const STANDING_SERIES: [&str; 4] = ["low", "medium", "hard", "xhard"];
/// A point averaging fewer tasks than this is marked thin.
const MIN_TASKS: usize = 3;
/// Tags one request may split on.
const MAX_TAGS: usize = 8;
/// Tags listed for the picker, most used first.
const AVAILABLE_TAGS: usize = 200;
/// How far before the oldest task's filing the invocation read starts.
const READ_SLACK: chrono::Duration = chrono::Duration::days(1);

#[derive(Deserialize, Default)]
pub(in crate::api) struct TurnsToEditQuery {
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    month: Option<String>,
    /// `done` (default) or `all` terminal statuses.
    #[serde(default)]
    outcome: Option<String>,
    /// Comma-separated complexities to draw; every one when absent.
    #[serde(default)]
    complexity: Option<String>,
    /// Comma-separated tags; a task carrying any of them is "with tag".
    #[serde(default)]
    tags: Option<String>,
    /// Only tasks whose first implement invocation ran as this agent.
    #[serde(default)]
    provider: Option<String>,
}

struct Filters {
    done_only: bool,
    /// `None` draws every complexity present.
    complexities: Option<BTreeSet<&'static str>>,
    tags: Vec<String>,
    provider: Option<String>,
}

fn comma_list(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_filters(q: &TurnsToEditQuery) -> Result<Filters, orbit_core::OrbitError> {
    let invalid = |message: String| orbit_core::OrbitError::InvalidInput(message);
    let done_only = match q.outcome.as_deref().unwrap_or("done") {
        "done" => true,
        "all" => false,
        other => {
            return Err(invalid(format!(
                "unknown outcome '{other}': expected done or all"
            )));
        }
    };
    let requested = comma_list(q.complexity.as_deref());
    let complexities = if requested.is_empty() {
        None
    } else {
        let mut set = BTreeSet::new();
        for value in &requested {
            let value = if value == "unassessed" {
                UNSET_BUCKET
            } else {
                value
            };
            let Some(known) = COMPLEXITIES.iter().find(|known| **known == value) else {
                return Err(invalid(format!(
                    "unknown complexity '{value}': expected low, medium, hard, xhard or unassessed"
                )));
            };
            set.insert(*known);
        }
        Some(set)
    };
    let mut tags = comma_list(q.tags.as_deref());
    tags.sort();
    tags.dedup();
    if tags.len() > MAX_TAGS {
        return Err(invalid(format!(
            "too many tags: at most {MAX_TAGS} may be compared"
        )));
    }
    let provider = q
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(str::to_string);
    Ok(Filters {
        done_only,
        complexities,
        tags,
        provider,
    })
}

/// `GET /api/diagnostics/turns_to_edit` — turns and result bytes before the
/// first file edit of each task's first implement invocation.
pub(in crate::api) async fn diagnostics_turns_to_edit(
    Ws(runtime): Ws,
    Query(q): Query<TurnsToEditQuery>,
) -> Response {
    let filters = match parse_filters(&q) {
        Ok(filters) => filters,
        Err(error) => return map_runtime_error(error),
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
    match super::super::blocking("turns to first edit", move || {
        turns_to_edit_trend(&runtime, &range, &filters)
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

/// What one task adds to the points it falls in.
#[derive(Clone, Copy)]
enum Contribution {
    Scored(u64, u64),
    /// The first implement invocation recorded no detectable edit.
    NoEdit,
    /// No implement invocation is linked to the task.
    NoInvocation,
}

/// The tasks one series point (or a series' window total) averages, and
/// those it leaves out.
#[derive(Clone, Default)]
struct Cell {
    samples: Vec<(u64, u64)>,
    no_edit: u64,
    no_invocation: u64,
}

impl Cell {
    fn record(&mut self, contribution: Contribution) {
        match contribution {
            Contribution::Scored(turns, bytes) => self.samples.push((turns, bytes)),
            Contribution::NoEdit => self.no_edit += 1,
            Contribution::NoInvocation => self.no_invocation += 1,
        }
    }

    fn is_empty(&self) -> bool {
        self.samples.is_empty() && self.no_edit == 0 && self.no_invocation == 0
    }

    fn to_json(&self) -> Value {
        let n = self.samples.len();
        let (mean_turns, median_turns) = average(self.samples.iter().map(|(turns, _)| *turns));
        let (mean_bytes, median_bytes) = average(self.samples.iter().map(|(_, bytes)| *bytes));
        json!({
            "n": n,
            "mean_turns": mean_turns,
            "median_turns": median_turns,
            "mean_result_bytes": mean_bytes,
            "median_result_bytes": median_bytes,
            "thin": n > 0 && n < MIN_TASKS,
            "excluded_no_edit": self.no_edit,
            "excluded_no_invocation": self.no_invocation,
        })
    }
}

/// Mean and median of `values`, or null when there are none. An even median
/// is the mean of the two central values.
fn average(values: impl Iterator<Item = u64>) -> (Value, Value) {
    let mut values: Vec<u64> = values.collect();
    let n = values.len();
    if n == 0 {
        return (Value::Null, Value::Null);
    }
    let sum: u128 = values.iter().map(|value| u128::from(*value)).sum();
    values.sort_unstable();
    let median = if n % 2 == 1 {
        values[n / 2] as f64
    } else {
        (values[n / 2 - 1] as f64 + values[n / 2] as f64) / 2.0
    };
    (json!(sum as f64 / n as f64), json!(median))
}

struct Series {
    points: Vec<Cell>,
    total: Cell,
}

fn turns_to_edit_trend(
    runtime: &OrbitRuntime,
    range: &DiagnosticsRange,
    filters: &Filters,
) -> Result<Value, orbit_core::OrbitError> {
    let transitions: Vec<_> = runtime
        .task_terminal_transitions(TrendWindow::read_since(range))?
        .into_iter()
        .filter(|row| !filters.done_only || row.status == TaskStatus::Done)
        .collect();
    let window = TrendWindow::new(range, transitions.first().map(|row| row.at));
    let in_window: Vec<_> = transitions
        .iter()
        .filter_map(|row| window.bucket_of(row.at).map(|index| (index, row)))
        .filter(|(_, row)| {
            filters
                .complexities
                .as_ref()
                .is_none_or(|selected| selected.contains(row.complexity.as_str()))
        })
        .collect();
    // No implement invocation predates its task, so the oldest task in the
    // window bounds the read, with a day's slack for clock skew. An owner
    // that omits `created_at` forces a full read.
    let read_since = in_window
        .iter()
        .map(|(_, row)| row.created_at)
        .collect::<Option<Vec<_>>>()
        .and_then(|created| created.into_iter().min())
        .map(|oldest| oldest - READ_SLACK);

    let (first_by_task, truncated) = if in_window.is_empty() {
        (HashMap::new(), false)
    } else {
        let edits = runtime.first_implement_edits(read_since)?;
        (edits.by_task, edits.truncated)
    };

    let cohorts: &[&str] = if filters.tags.is_empty() {
        &["all"]
    } else {
        &["with", "without"]
    };
    let bucket_count = window.bucket_count();
    let mut series: BTreeMap<(String, &str), Series> = BTreeMap::new();
    let standing = STANDING_SERIES.iter().filter(|complexity| {
        filters
            .complexities
            .as_ref()
            .is_none_or(|selected| selected.contains(*complexity))
    });
    for complexity in standing {
        for cohort in cohorts {
            series.insert(
                (complexity.to_string(), cohort),
                Series {
                    points: vec![Cell::default(); bucket_count],
                    total: Cell::default(),
                },
            );
        }
    }
    let mut tag_counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut provider_counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut overall = Cell::default();
    let mut other_provider = 0u64;
    for (index, row) in &in_window {
        for tag in &row.tags {
            *tag_counts.entry(tag.as_str()).or_default() += 1;
        }
        let first = first_by_task.get(&row.task_id);
        if let Some(first) = first
            && !first.agent.is_empty()
        {
            *provider_counts.entry(first.agent.clone()).or_default() += 1;
        }
        if let Some(wanted) = filters.provider.as_deref()
            && let Some(first) = first
            && first.agent != wanted
        {
            other_provider += 1;
            continue;
        }
        let cohort = if filters.tags.is_empty() {
            "all"
        } else if row.tags.iter().any(|tag| filters.tags.contains(tag)) {
            "with"
        } else {
            "without"
        };
        let contribution = match first {
            None => Contribution::NoInvocation,
            Some(first) => match first.score {
                Some((turns, bytes)) => Contribution::Scored(turns, bytes),
                None => Contribution::NoEdit,
            },
        };
        let entry = series
            .entry((row.complexity.clone(), cohort))
            .or_insert_with(|| Series {
                points: vec![Cell::default(); bucket_count],
                total: Cell::default(),
            });
        entry.points[*index].record(contribution);
        entry.total.record(contribution);
        overall.record(contribution);
    }

    let mut series: Vec<_> = series.into_iter().collect();
    series.sort_by(|((left, left_cohort), _), ((right, right_cohort), _)| {
        complexity_bucket_ord(left)
            .cmp(&complexity_bucket_ord(right))
            .then_with(|| left_cohort.cmp(right_cohort))
    });
    let series: Vec<Value> = series
        .into_iter()
        .filter(|((complexity, _), series)| {
            STANDING_SERIES.contains(&complexity.as_str()) || !series.total.is_empty()
        })
        .map(|((complexity, cohort), series)| {
            json!({
                "complexity": complexity,
                "cohort": cohort,
                "points": series.points.iter().map(Cell::to_json).collect::<Vec<_>>(),
                "total": series.total.to_json(),
            })
        })
        .collect();
    let mut tags: Vec<(&str, u64)> = tag_counts.into_iter().collect();
    tags.sort_by(|(left, left_n), (right, right_n)| right_n.cmp(left_n).then(left.cmp(right)));
    tags.truncate(AVAILABLE_TAGS);
    let mut providers: Vec<(String, u64)> = provider_counts.into_iter().collect();
    providers.sort_by(|(left, left_n), (right, right_n)| {
        right_n.cmp(left_n).then_with(|| left.cmp(right))
    });

    Ok(json!({
        "window": window.to_json(),
        "outcome": if filters.done_only { "done" } else { "all" },
        "provider": filters.provider,
        "tags": filters.tags,
        "min_tasks": MIN_TASKS,
        "truncated": truncated,
        "bucket_starts": (0..bucket_count)
            .map(|index| window.bucket_start(index).to_rfc3339())
            .collect::<Vec<_>>(),
        "series": series,
        "total": overall.to_json(),
        "excluded_other_provider": other_provider,
        "available_tags": tags
            .into_iter()
            .map(|(tag, tasks)| json!({ "tag": tag, "tasks": tasks }))
            .collect::<Vec<_>>(),
        "available_providers": providers
            .into_iter()
            .map(|(provider, tasks)| json!({ "provider": provider, "tasks": tasks }))
            .collect::<Vec<_>>(),
    }))
}
