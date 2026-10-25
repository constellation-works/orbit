//! Average normalized tokens per task over time, by complexity and tag
//! cohort, for Analytics › Metrics.
//!
//! Each terminal task counts once, in the bucket of its index `updated_at`
//! (the completions series uses the same time; a later comment or artifact
//! on the terminal bundle moves it). Its total is every managed invocation
//! linked to it, whenever that invocation ran, so spend recorded while the
//! task was in flight never lands in an earlier bucket.

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::OrbitRuntime;
use orbit_core::metrics::cost::{TaskTokenSpend, TaskTokenTotals};
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
const SPEND_READ_SLACK: chrono::Duration = chrono::Duration::days(1);

#[derive(Deserialize, Default)]
pub(in crate::api) struct TaskTokensQuery {
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    month: Option<String>,
    /// `done` (default) or `all` terminal statuses.
    #[serde(default)]
    outcome: Option<String>,
    /// `all` (default) linked invocations or `implement` ones only.
    #[serde(default)]
    scope: Option<String>,
    /// Comma-separated complexities to draw; every one when absent.
    #[serde(default)]
    complexity: Option<String>,
    /// Comma-separated tags; a task carrying any of them is "with tag".
    #[serde(default)]
    tags: Option<String>,
    /// Only tasks whose latest implement invocation ran this model.
    #[serde(default)]
    model: Option<String>,
}

struct Filters {
    done_only: bool,
    implement_only: bool,
    /// `None` draws every complexity present.
    complexities: Option<BTreeSet<&'static str>>,
    tags: Vec<String>,
    model: Option<String>,
}

fn comma_list(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_filters(q: &TaskTokensQuery) -> Result<Filters, orbit_core::OrbitError> {
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
    let implement_only = match q.scope.as_deref().unwrap_or("all") {
        "all" => false,
        "implement" => true,
        other => {
            return Err(invalid(format!(
                "unknown scope '{other}': expected all or implement"
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
    let model = q
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string);
    Ok(Filters {
        done_only,
        implement_only,
        complexities,
        tags,
        model,
    })
}

pub(in crate::api) async fn diagnostics_task_tokens(
    Ws(runtime): Ws,
    Query(q): Query<TaskTokensQuery>,
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
    match super::super::blocking("task token trend", move || {
        task_token_trend(&runtime, &range, &filters)
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
    Tokens(u64),
    /// An invocation in scope has no covered model, so its total is unknown.
    Uncovered,
    /// No invocation in scope is linked to the task.
    NoInvocations,
}

impl Contribution {
    fn of(spend: Option<&TaskTokenSpend>) -> Self {
        match spend {
            Some(spend) if spend.invocation_count == 0 => Self::NoInvocations,
            Some(spend) if spend.uncovered_invocation_count > 0 => Self::Uncovered,
            Some(spend) => Self::Tokens(spend.normalized_token_total),
            None => Self::NoInvocations,
        }
    }
}

/// The tasks one series point (or a series' window total) averages, and
/// those it leaves out.
#[derive(Clone, Default)]
struct Cell {
    tokens: Vec<u64>,
    uncovered: u64,
    no_invocations: u64,
}

impl Cell {
    fn record(&mut self, contribution: Contribution) {
        match contribution {
            Contribution::Tokens(tokens) => self.tokens.push(tokens),
            Contribution::Uncovered => self.uncovered += 1,
            Contribution::NoInvocations => self.no_invocations += 1,
        }
    }

    fn is_empty(&self) -> bool {
        self.tokens.is_empty() && self.uncovered == 0 && self.no_invocations == 0
    }

    fn to_json(&self) -> Value {
        let mut tokens = self.tokens.clone();
        tokens.sort_unstable();
        let n = tokens.len();
        let (mean, median) = if n == 0 {
            (Value::Null, Value::Null)
        } else {
            let sum: u128 = tokens.iter().map(|tokens| u128::from(*tokens)).sum();
            let middle = if n % 2 == 1 {
                tokens[n / 2] as f64
            } else {
                (tokens[n / 2 - 1] as f64 + tokens[n / 2] as f64) / 2.0
            };
            (json!(sum as f64 / n as f64), json!(middle))
        };
        json!({
            "n": n,
            "mean": mean,
            "median": median,
            "thin": n > 0 && n < MIN_TASKS,
            "excluded_uncovered": self.uncovered,
            "excluded_no_invocations": self.no_invocations,
        })
    }
}

struct Series {
    points: Vec<Cell>,
    total: Cell,
}

fn task_token_trend(
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
    // No linked invocation predates its task, so the oldest task in the
    // window bounds the invocation read, with a day's slack for clock skew
    // between the hosts that stamp the two; an owner that omits `created_at`
    // forces a full read.
    let spend_since = in_window
        .iter()
        .map(|(_, row)| row.created_at)
        .collect::<Option<Vec<_>>>()
        .and_then(|created| created.into_iter().min())
        .map(|oldest| oldest - SPEND_READ_SLACK);
    let totals: BTreeMap<String, TaskTokenTotals> = if in_window.is_empty() {
        BTreeMap::new()
    } else {
        runtime.task_token_totals(spend_since)?
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
    let mut model_counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut overall = Cell::default();
    let mut other_model = 0u64;
    for (index, row) in &in_window {
        let task = totals.get(&row.task_id);
        for tag in &row.tags {
            *tag_counts.entry(tag.as_str()).or_default() += 1;
        }
        let implement_model = task.and_then(|task| task.implement_model.as_deref());
        if let Some(model) = implement_model {
            *model_counts.entry(model).or_default() += 1;
        }
        if filters
            .model
            .as_deref()
            .is_some_and(|wanted| implement_model != Some(wanted))
        {
            other_model += 1;
            continue;
        }
        let cohort = if filters.tags.is_empty() {
            "all"
        } else if row.tags.iter().any(|tag| filters.tags.contains(tag)) {
            "with"
        } else {
            "without"
        };
        let contribution = Contribution::of(task.map(|task| {
            if filters.implement_only {
                &task.implement
            } else {
                &task.all
            }
        }));
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
    let mut models: Vec<(&str, u64)> = model_counts.into_iter().collect();
    models.sort_by(|(left, left_n), (right, right_n)| right_n.cmp(left_n).then(left.cmp(right)));

    Ok(json!({
        "window": window.to_json(),
        "outcome": if filters.done_only { "done" } else { "all" },
        "scope": if filters.implement_only { "implement" } else { "all" },
        "tags": filters.tags,
        "model": filters.model,
        "min_tasks": MIN_TASKS,
        "bucket_starts": (0..bucket_count)
            .map(|index| window.bucket_start(index).to_rfc3339())
            .collect::<Vec<_>>(),
        "series": series,
        "total": overall.to_json(),
        "excluded_other_model": other_model,
        "available_tags": tags
            .into_iter()
            .map(|(tag, tasks)| json!({ "tag": tag, "tasks": tasks }))
            .collect::<Vec<_>>(),
        "available_models": models
            .into_iter()
            .map(|(model, tasks)| json!({ "model": model, "tasks": tasks }))
            .collect::<Vec<_>>(),
    }))
}
