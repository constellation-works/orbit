//! Average normalized tokens per task over time, by complexity and tag
//! cohort, for Analytics › Metrics.
//!
//! Each terminal task counts once, in the bucket of its durable terminal
//! transition (the completions series uses the same time; a later write on the
//! terminal bundle does not move it). A task whose history has no such
//! transition is left out and counted in `undated_tasks`. Its total is every
//! managed invocation linked to it, whenever that invocation ran, so spend
//! recorded while the task was in flight never lands in an earlier bucket.

use std::collections::BTreeMap;

use axum::extract::{Query, State};
use axum::response::Response;
use orbit_core::OrbitRuntime;
use orbit_core::metrics::cost::{TaskTokenSpend, TaskTokenTotals};
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::map_runtime_error;
use super::metrics::DiagnosticsRange;
use super::trend::{
    CohortCell, CohortFilters, CohortSeriesMap, CohortWindowData, MIN_TASKS, available_tags_json,
    average, memoized_trend_response, sorted_counts, trimmed_non_empty,
};
use crate::state::{DashboardState, Ws};

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
    cohort: CohortFilters,
    implement_only: bool,
    model: Option<String>,
}

fn parse_filters(q: &TaskTokensQuery) -> Result<Filters, orbit_core::OrbitError> {
    let cohort = CohortFilters::parse(
        q.outcome.as_deref(),
        q.complexity.as_deref(),
        q.tags.as_deref(),
    )?;
    let implement_only = match q.scope.as_deref().unwrap_or("all") {
        "all" => false,
        "implement" => true,
        other => {
            return Err(orbit_core::OrbitError::InvalidInput(format!(
                "unknown scope '{other}': expected all or implement"
            )));
        }
    };
    let model = trimmed_non_empty(q.model.as_deref());
    Ok(Filters {
        cohort,
        implement_only,
        model,
    })
}

pub(in crate::api) async fn diagnostics_task_tokens(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(q): Query<TaskTokensQuery>,
) -> Response {
    let filters = match parse_filters(&q) {
        Ok(filters) => filters,
        Err(error) => return map_runtime_error(error),
    };
    // Normalized key over every parameter that changes the result. Filter
    // parsing above rejects invalid inputs before the memo, so errors are
    // never cached.
    memoized_trend_response(
        &state,
        runtime,
        "task_tokens",
        (q.since, q.month),
        filters,
        |range, filters| {
            let complexities = filters.cohort.complexities_key();
            let scope = if filters.implement_only {
                "implement"
            } else {
                "all"
            };
            serde_json::to_string(&(
                &range.key,
                filters.cohort.outcome_label(),
                scope,
                &complexities,
                &filters.cohort.tags,
                &filters.model,
            ))
        },
        task_token_trend,
    )
    .await
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
}

impl CohortCell for Cell {
    fn is_empty(&self) -> bool {
        self.tokens.is_empty() && self.uncovered == 0 && self.no_invocations == 0
    }

    fn to_json(&self) -> Value {
        let n = self.tokens.len();
        let (mean, median) = average(self.tokens.iter().copied());
        json!({
            "n": n,
            "sum": self.tokens.iter().map(|tokens| u128::from(*tokens)).sum::<u128>() as f64,
            "mean": mean,
            "median": median,
            "thin": n > 0 && n < MIN_TASKS,
            "excluded_uncovered": self.uncovered,
            "excluded_no_invocations": self.no_invocations,
        })
    }
}

fn task_token_trend(
    runtime: &OrbitRuntime,
    range: &DiagnosticsRange,
    filters: &Filters,
) -> Result<Value, orbit_core::OrbitError> {
    let cohort_window = CohortWindowData::load(runtime, range, &filters.cohort, true)?;
    let totals: BTreeMap<String, TaskTokenTotals> = if cohort_window.in_window.is_empty() {
        BTreeMap::new()
    } else {
        runtime.task_token_totals(cohort_window.invocation_read_since())?
    };

    let mut series = CohortSeriesMap::<Cell>::new(&filters.cohort, &cohort_window.window);
    let mut tag_counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut model_counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut overall = Cell::default();
    let mut other_model = 0u64;
    let contribution_for = |task: Option<&TaskTokenTotals>| {
        Contribution::of(task.map(|task| {
            if filters.implement_only {
                &task.implement
            } else {
                &task.all
            }
        }))
    };
    for (index, row) in &cohort_window.in_window {
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
        let cohort = filters.cohort.cohort_of(&row.tags);
        let contribution = contribution_for(task);
        let entry = series.entry_mut(&row.complexity, cohort);
        entry.points[*index].record(contribution);
        entry.total.record(contribution);
        overall.record(contribution);
    }
    for (index, row) in &cohort_window.lead_rows {
        let task = totals.get(&row.task_id);
        let implement_model = task.and_then(|task| task.implement_model.as_deref());
        if filters
            .model
            .as_deref()
            .is_some_and(|wanted| implement_model != Some(wanted))
        {
            continue;
        }
        let cohort = filters.cohort.cohort_of(&row.tags);
        series.entry_mut(&row.complexity, cohort).lead_points[*index]
            .record(contribution_for(task));
    }

    let models = sorted_counts(model_counts);
    Ok(json!({
        "window": cohort_window.window.to_json(),
        "outcome": filters.cohort.outcome_label(),
        "scope": if filters.implement_only { "implement" } else { "all" },
        "tags": filters.cohort.tags,
        "model": filters.model,
        "min_tasks": MIN_TASKS,
        "bucket_starts": cohort_window.bucket_starts_json(),
        "series": series.into_json(true),
        "total": overall.to_json(),
        "excluded_other_model": other_model,
        "undated_tasks": cohort_window.undated_tasks,
        "available_tags": available_tags_json(tag_counts),
        "available_models": models
            .into_iter()
            .map(|(model, tasks)| json!({ "model": model, "tasks": tasks }))
            .collect::<Vec<_>>(),
    }))
}
