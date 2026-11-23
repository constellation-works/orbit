//! Tool calls and result bytes before the first file edit, by complexity
//! and tag cohort, for Analytics › Metrics.
//!
//! Each terminal task counts once, in the bucket of its durable terminal
//! transition, as in the completions series; a task whose history has no such
//! transition is left out and counted in `undated_tasks`.
//! Its score is the first-edit score of its first implement invocation, the
//! same definition the experiment readout uses. Tasks without a score are
//! excluded and counted, never averaged as zero. Empty traces are counted
//! separately from populated traces with no recognized editor call; a shell
//! such as `command_execution` is not an edit and is never averaged as zero.
//! A trace whose provider output outgrew the bounded capture holds only a
//! suffix of the invocation's calls, so it has no first-edit score either: it
//! is counted as an incomplete trace, never as a measurement.

use std::collections::{BTreeMap, HashMap};

use axum::extract::{Query, State};
use axum::response::Response;
use orbit_core::OrbitRuntime;
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::map_runtime_error;
use super::metrics::DiagnosticsRange;
use super::trend::{
    CohortCell, CohortFilters, CohortSeriesMap, CohortWindowData, MIN_TASKS, available_tags_json,
    average, memoized_trend_response, trimmed_non_empty,
};
use crate::state::{DashboardState, Ws};

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
    cohort: CohortFilters,
    provider: Option<String>,
}

fn parse_filters(q: &TurnsToEditQuery) -> Result<Filters, orbit_core::OrbitError> {
    let cohort = CohortFilters::parse(
        q.outcome.as_deref(),
        q.complexity.as_deref(),
        q.tags.as_deref(),
    )?;
    let provider = trimmed_non_empty(q.provider.as_deref());
    Ok(Filters { cohort, provider })
}

/// `GET /api/diagnostics/turns_to_edit` — turns and result bytes before the
/// first file edit of each task's first implement invocation.
pub(in crate::api) async fn diagnostics_turns_to_edit(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(q): Query<TurnsToEditQuery>,
) -> Response {
    let filters = match parse_filters(&q) {
        Ok(filters) => filters,
        Err(error) => return map_runtime_error(error),
    };
    memoized_trend_response(
        &state,
        runtime,
        "turns_to_edit",
        (q.since, q.month),
        filters,
        |range, filters| {
            let complexities = filters.cohort.complexities_key();
            serde_json::to_string(&(
                &range.key,
                filters.cohort.outcome_label(),
                &complexities,
                &filters.cohort.tags,
                &filters.provider,
            ))
        },
        turns_to_edit_trend,
    )
    .await
}

/// What one task adds to the points it falls in.
#[derive(Clone, Copy)]
enum Contribution {
    Scored(u64, u64),
    /// The first implement invocation has no tool calls to inspect.
    EmptyTrace,
    /// The first implement trace lost its early calls to the capture bound, so
    /// no editor call in it is known to be the first.
    IncompleteTrace,
    /// The populated first implement trace has no recognized editor call.
    NoDetectableEdit,
    /// No implement invocation is linked to the task.
    NoInvocation,
}

/// The tasks one series point (or a series' window total) averages, and
/// those it leaves out.
#[derive(Clone, Default)]
struct Cell {
    samples: Vec<(u64, u64)>,
    empty_trace: u64,
    incomplete_trace: u64,
    no_detectable_edit: u64,
    no_invocation: u64,
}

impl Cell {
    fn record(&mut self, contribution: Contribution) {
        match contribution {
            Contribution::Scored(turns, bytes) => self.samples.push((turns, bytes)),
            Contribution::EmptyTrace => self.empty_trace += 1,
            Contribution::IncompleteTrace => self.incomplete_trace += 1,
            Contribution::NoDetectableEdit => self.no_detectable_edit += 1,
            Contribution::NoInvocation => self.no_invocation += 1,
        }
    }

    fn task_count(&self) -> u64 {
        self.samples.len() as u64
            + self.empty_trace
            + self.incomplete_trace
            + self.no_detectable_edit
            + self.no_invocation
    }

    fn coverage_json(&self) -> Value {
        let measured = self.samples.len() as u64;
        let tasks = self.task_count();
        let invocations = tasks - self.no_invocation;
        let trace_rate = if invocations == 0 {
            Value::Null
        } else {
            json!((invocations - self.empty_trace) as f64 / invocations as f64)
        };
        let measured_rate = if tasks == 0 {
            Value::Null
        } else {
            json!(measured as f64 / tasks as f64)
        };
        json!({
            "tasks": tasks,
            "measured": measured,
            "empty_trace": self.empty_trace,
            "incomplete_trace": self.incomplete_trace,
            "no_detectable_edit": self.no_detectable_edit,
            "no_invocation": self.no_invocation,
            "trace_rate": trace_rate,
            "measured_rate": measured_rate,
        })
    }
}

impl CohortCell for Cell {
    fn is_empty(&self) -> bool {
        self.samples.is_empty()
            && self.empty_trace == 0
            && self.incomplete_trace == 0
            && self.no_detectable_edit == 0
            && self.no_invocation == 0
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
            // Keep the previous aggregate field for existing clients.
            "excluded_no_edit": self.empty_trace + self.no_detectable_edit,
            "excluded_empty_trace": self.empty_trace,
            "excluded_incomplete_trace": self.incomplete_trace,
            "excluded_no_detectable_edit": self.no_detectable_edit,
            "excluded_no_invocation": self.no_invocation,
            "tasks": self.task_count(),
        })
    }
}

fn turns_to_edit_trend(
    runtime: &OrbitRuntime,
    range: &DiagnosticsRange,
    filters: &Filters,
) -> Result<Value, orbit_core::OrbitError> {
    let cohort_window = CohortWindowData::load(runtime, range, &filters.cohort, false)?;
    let (first_by_task, truncated) = if cohort_window.in_window.is_empty() {
        (HashMap::new(), false)
    } else {
        let edits = runtime.first_implement_edits(cohort_window.invocation_read_since())?;
        (edits.by_task, edits.truncated)
    };

    let mut series = CohortSeriesMap::<Cell>::new(&filters.cohort, &cohort_window.window);
    let mut tag_counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut provider_coverage: BTreeMap<String, Cell> = BTreeMap::new();
    let mut provider_summary: BTreeMap<String, Cell> = BTreeMap::new();
    let mut overall = Cell::default();
    let mut other_provider = 0u64;
    let mut unattributed = 0u64;
    for (index, row) in &cohort_window.in_window {
        for tag in &row.tags {
            *tag_counts.entry(tag.as_str()).or_default() += 1;
        }
        let first = first_by_task.get(&row.task_id);
        let contribution = match first {
            None => Contribution::NoInvocation,
            Some(first) => match first.score {
                Some((turns, bytes)) => Contribution::Scored(turns, bytes),
                None if first.incomplete_trace => Contribution::IncompleteTrace,
                None if first.empty_trace => Contribution::EmptyTrace,
                None => Contribution::NoDetectableEdit,
            },
        };
        if let Some(first) = first
            && !first.agent.is_empty()
        {
            provider_coverage
                .entry(first.agent.clone())
                .or_default()
                .record(contribution);
        }
        if let Some(first) = first
            && !first.agent.is_empty()
            && (filters.cohort.tags.is_empty()
                || row.tags.iter().any(|tag| filters.cohort.tags.contains(tag)))
        {
            provider_summary
                .entry(first.agent.clone())
                .or_default()
                .record(contribution);
        }
        if let Some(wanted) = filters.provider.as_deref() {
            match first {
                Some(first) if first.agent == wanted => {}
                Some(_) => {
                    other_provider += 1;
                    continue;
                }
                None => {
                    unattributed += 1;
                    continue;
                }
            }
        }
        let cohort = filters.cohort.cohort_of(&row.tags);
        let entry = series.entry_mut(&row.complexity, cohort);
        entry.points[*index].record(contribution);
        entry.total.record(contribution);
        overall.record(contribution);
    }

    let mut providers: Vec<(String, Cell)> = provider_coverage.into_iter().collect();
    providers.sort_by(|(left, left_coverage), (right, right_coverage)| {
        right_coverage
            .task_count()
            .cmp(&left_coverage.task_count())
            .then_with(|| left.cmp(right))
    });

    Ok(json!({
        "window": cohort_window.window.to_json(),
        "outcome": filters.cohort.outcome_label(),
        "provider": filters.provider,
        "tags": filters.cohort.tags,
        "min_tasks": MIN_TASKS,
        "truncated": truncated,
        "bucket_starts": cohort_window.bucket_starts_json(),
        "series": series.into_json(false),
        "total": overall.to_json(),
        "coverage": overall.coverage_json(),
        "excluded_other_provider": other_provider,
        "excluded_unattributed": unattributed,
        "undated_tasks": cohort_window.undated_tasks,
        "available_tags": available_tags_json(tag_counts),
        "provider_summary": provider_summary.into_iter().map(|(provider, cell)| {
            let (mean_turns, median_turns) = average(cell.samples.iter().map(|(turns, _)| *turns));
            let n = cell.samples.len();
            json!({
                "provider": provider, "n": n, "mean_turns": mean_turns,
                "median_turns": median_turns, "measured_rate": cell.coverage_json()["measured_rate"],
                "thin": n < MIN_TASKS,
            })
        }).collect::<Vec<_>>(),
        "available_providers": providers
            .into_iter()
            .map(|(provider, coverage)| {
                json!({
                    "provider": provider,
                    "tasks": coverage.task_count(),
                    "coverage": coverage.coverage_json(),
                })
            })
            .collect::<Vec<_>>(),
    }))
}
