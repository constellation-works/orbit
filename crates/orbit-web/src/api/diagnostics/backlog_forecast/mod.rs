//! Backlog forecast for Analytics › Forecast [ORB-18385]: how long the
//! dispatchable backlog takes to drain, and what it costs in USD and
//! normalized tokens, as p50 and p90 from resampled delivery history, plus
//! the calibration of earlier stored forecasts.
//!
//! The method is documented in `docs/design/backlog-forecast/1_scope.md`. In short:
//!
//! - The sampling unit is one completed delivery: a task that reached
//!   `done` inside the lookback, with every managed invocation linked to it
//!   (recoveries, auto-fixes and reworks included). Its hours are the union
//!   of those invocations' run intervals, bridging gaps shorter than
//!   [`GAP_BRIDGE_HOURS`]; its tokens and USD are their normalized token
//!   and provider cost sums.
//! - Each backlog task draws from its complexity × crew-pool cell, falling
//!   back to its complexity's whole history below
//!   [`model::MIN_CELL_SAMPLES`], and to every delivery when it is
//!   unassessed or its complexity has none.
//! - The drain is simulated on the drain's leaf-run ceiling
//!   ([`OrbitRuntime::drain_concurrency_ceiling`]), with no new arrivals
//!   unless `arrivals=on`.
//!
//! The canonical forecast (default lookback, the live ceiling, no arrivals)
//! is stored at most once per workspace per [`STORE_INTERVAL_MINUTES`], and
//! every request scores the newest stored forecasts.

mod model;
mod scoring;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Duration, Utc};
use orbit_core::{OrbitRuntime, StoredBacklogForecast};
use orbit_types::task::{
    Task, TaskPriority, TaskStatus, UNSET_BUCKET, auto_task_template, complexity_bucket,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::map_runtime_error;
use crate::state::Ws;
use model::{Arrivals, Basis, Delivery, Item, Resolved, Spent};

/// Default history lookback, in days.
const DEFAULT_LOOKBACK_DAYS: i64 = 30;
const MIN_LOOKBACK_DAYS: i64 = 7;
const MAX_LOOKBACK_DAYS: i64 = 180;
/// The most leaf runs a request may simulate.
const MAX_CONCURRENCY: u32 = 64;
/// Days the arrival rate is measured over.
const ARRIVAL_DAYS: i64 = 7;
/// Gaps between a task's invocations shorter than this count as delivery
/// time (CI, review and gate waits inside a run); longer ones (the task
/// waiting in the backlog between attempts) do not.
const GAP_BRIDGE_HOURS: f64 = 2.0;
/// At most one stored forecast per workspace per this many minutes.
const STORE_INTERVAL_MINUTES: i64 = 60;
/// Stored forecasts the scoring pass reads, newest first.
const SCORED_FORECASTS: usize = 60;
/// How far before the oldest task or forecast the invocation read starts.
const SPEND_READ_SLACK: Duration = Duration::days(1);
/// Tags whose filings count as sweep output rather than hand filings.
const SWEEP_TAGS: [&str; 7] = [
    "ci-failure-sweep",
    "qa-sweep",
    "code-review",
    "security-review",
    "delivery-code-review",
    "friction-curation",
    "baseline-red-repair",
];

#[derive(Deserialize, Default)]
pub(in crate::api) struct BacklogForecastQuery {
    /// History lookback in days (7-180, default 30).
    #[serde(default)]
    lookback_days: Option<i64>,
    /// Leaf runs to simulate; the live drain's ceiling when absent.
    #[serde(default)]
    concurrency: Option<u32>,
    /// `on` adds new tasks at the recent arrival rate.
    #[serde(default)]
    arrivals: Option<String>,
}

struct Options {
    lookback_days: i64,
    concurrency: Option<u32>,
    arrivals: bool,
}

impl Options {
    /// Only the canonical forecast is stored, so calibration compares like
    /// with like.
    fn canonical(&self) -> bool {
        self.lookback_days == DEFAULT_LOOKBACK_DAYS && self.concurrency.is_none() && !self.arrivals
    }
}

fn parse_options(q: &BacklogForecastQuery) -> Result<Options, orbit_core::OrbitError> {
    let invalid = |message: String| orbit_core::OrbitError::InvalidInput(message);
    let lookback_days = q.lookback_days.unwrap_or(DEFAULT_LOOKBACK_DAYS);
    if !(MIN_LOOKBACK_DAYS..=MAX_LOOKBACK_DAYS).contains(&lookback_days) {
        return Err(invalid(format!(
            "lookback_days must be between {MIN_LOOKBACK_DAYS} and {MAX_LOOKBACK_DAYS}"
        )));
    }
    if let Some(concurrency) = q.concurrency
        && !(1..=MAX_CONCURRENCY).contains(&concurrency)
    {
        return Err(invalid(format!(
            "concurrency must be between 1 and {MAX_CONCURRENCY}"
        )));
    }
    let arrivals = match q.arrivals.as_deref().unwrap_or("off") {
        "off" => false,
        "on" => true,
        other => {
            return Err(invalid(format!(
                "unknown arrivals '{other}': expected on or off"
            )));
        }
    };
    Ok(Options {
        lookback_days,
        concurrency: q.concurrency,
        arrivals,
    })
}

pub(in crate::api) async fn diagnostics_backlog_forecast(
    Ws(runtime): Ws,
    Query(q): Query<BacklogForecastQuery>,
) -> Response {
    let options = match parse_options(&q) {
        Ok(options) => options,
        Err(error) => return map_runtime_error(error),
    };
    match super::super::blocking("backlog forecast", move || {
        backlog_forecast(&runtime, &options, Utc::now())
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

/// One managed invocation linked to a task: when it ran and what it used.
pub(super) struct Run {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    tokens: Option<f64>,
    usd: Option<f64>,
}

/// What a task's invocations used, from some time on.
pub(super) struct Usage {
    pub(super) invocations: usize,
    pub(super) hours: f64,
    /// `None` when an invocation ran an uncovered model.
    pub(super) tokens: Option<f64>,
    /// `None` when an invocation recorded no valid provider cost.
    pub(super) usd: Option<f64>,
    pub(super) last_end: Option<DateTime<Utc>>,
}

/// The usage of `runs` (sorted by start) that ended after `after`; a run
/// that started before `after` counts from it.
pub(super) fn usage(runs: &[Run], after: Option<DateTime<Utc>>) -> Usage {
    let bridge = Duration::seconds((GAP_BRIDGE_HOURS * 3600.0) as i64);
    let mut used = Usage {
        invocations: 0,
        hours: 0.0,
        tokens: Some(0.0),
        usd: Some(0.0),
        last_end: None,
    };
    let mut open: Option<(DateTime<Utc>, DateTime<Utc>)> = None;
    let mut busy = Duration::zero();
    for run in runs {
        if after.is_some_and(|after| run.end <= after) {
            continue;
        }
        let start = after.map_or(run.start, |after| run.start.max(after));
        used.invocations += 1;
        used.tokens = used.tokens.zip(run.tokens).map(|(sum, one)| sum + one);
        used.usd = used.usd.zip(run.usd).map(|(sum, one)| sum + one);
        used.last_end = used.last_end.max(Some(run.end));
        open = match open {
            Some((from, to)) if start <= to + bridge => Some((from, to.max(run.end))),
            Some((from, to)) => {
                busy += to - from;
                Some((start, run.end))
            }
            None => Some((start, run.end)),
        };
    }
    if let Some((from, to)) = open {
        busy += to - from;
    }
    used.hours = busy.num_seconds().max(0) as f64 / 3600.0;
    used
}

fn priority_rank(priority: TaskPriority) -> u8 {
    match priority {
        TaskPriority::Critical => 0,
        TaskPriority::High => 1,
        TaskPriority::Medium => 2,
        TaskPriority::Low => 3,
    }
}

/// Complexity tier → the crews its pool draws (weight above zero).
fn complexity_pools(runtime: &OrbitRuntime) -> BTreeMap<String, BTreeSet<String>> {
    let pools = match runtime.workspace_complexity_crew_pools() {
        Ok(pools) => pools,
        Err(error) => {
            tracing::warn!(%error, "backlog forecast: crew pools unreadable");
            return BTreeMap::new();
        }
    };
    let Some(pools) = pools.as_object() else {
        return BTreeMap::new();
    };
    pools
        .iter()
        .map(|(tier, pool)| {
            let crews = pool
                .get("crews")
                .and_then(Value::as_array)
                .map(|crews| {
                    crews
                        .iter()
                        .filter(|entry| entry.get("weight").and_then(Value::as_u64) != Some(0))
                        .filter_map(|entry| entry.get("name").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            (tier.clone(), crews)
        })
        .collect()
}

/// Where a recent filing came from.
fn arrival_source(task: &Task) -> &'static str {
    if auto_task_template(&task.tags).is_some() {
        "auto_task"
    } else if task
        .tags
        .iter()
        .any(|tag| SWEEP_TAGS.contains(&tag.as_str()))
    {
        "sweep"
    } else {
        "filing"
    }
}

fn backlog_forecast(
    runtime: &OrbitRuntime,
    options: &Options,
    now: DateTime<Utc>,
) -> Result<Value, orbit_core::OrbitError> {
    let lookback_start = now - Duration::days(options.lookback_days);
    let arrival_start = now - Duration::days(ARRIVAL_DAYS);
    let tasks: HashMap<String, Task> = runtime
        .list_task_metadata()?
        .into_iter()
        .map(|task| (task.id.to_string(), task))
        .collect();
    let stored = runtime.backlog_forecasts(SCORED_FORECASTS)?;
    let oldest_forecast = stored.iter().map(|forecast| forecast.forecast_at).min();
    let transitions_since = [Some(lookback_start), Some(arrival_start), oldest_forecast]
        .into_iter()
        .flatten()
        .min();
    let mut done_at: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut history_rows = Vec::new();
    // The index dates terminal rows by updated_at, which later comments or
    // artifacts can advance. Use it only to narrow candidates: an actual
    // completion cannot be newer than its last update. Read each candidate's
    // event log once, using the latest entry into done (including creation
    // in done) for both history membership and calibration. Never substitute
    // mutable metadata when an older task has no recorded done event.
    for row in runtime
        .task_terminal_transitions(transitions_since)?
        .into_iter()
        .filter(|row| row.status == TaskStatus::Done)
    {
        let task_history = match runtime.get_task_history(&row.task_id) {
            Ok(history) => history,
            Err(error) => {
                tracing::warn!(
                    task_id = %row.task_id,
                    %error,
                    "backlog forecast: task history unreadable"
                );
                continue;
            }
        };
        let completed_at = task_history
            .into_iter()
            .rev()
            .find(|entry| entry.to_status == Some(TaskStatus::Done))
            .map(|entry| entry.at);
        if let Some(at) = completed_at {
            done_at.insert(row.task_id.clone(), at);
            if at >= lookback_start {
                history_rows.push(row);
            }
        }
    }

    // The invocation read starts at the earliest of the lookback, the oldest
    // task it must cover and the oldest stored forecast. Creation times
    // alone are not relied on as the bound, since nothing guarantees a task
    // record's creation time precedes the runs linked to it; the lookback
    // start costs little, as the oldest history task usually predates it.
    let in_progress_created = tasks
        .values()
        .filter(|task| task.status == TaskStatus::InProgress)
        .map(|task| task.created_at);
    let spend_since = history_rows
        .iter()
        .filter_map(|row| row.created_at)
        .chain(in_progress_created)
        .chain(oldest_forecast)
        .chain((!history_rows.is_empty()).then_some(lookback_start))
        .min()
        .map(|oldest| oldest - SPEND_READ_SLACK);
    let mut runs: HashMap<String, Vec<Run>> = HashMap::new();
    if spend_since.is_some() {
        for spend in runtime.invocation_spend(spend_since)? {
            let end = spend.ts;
            let start = end - Duration::milliseconds(i64::try_from(spend.duration_ms).unwrap_or(0));
            for task_id in &spend.task_ids {
                runs.entry(task_id.clone()).or_default().push(Run {
                    start,
                    end,
                    tokens: spend.normalized_tokens.map(|tokens| tokens as f64),
                    usd: spend.provider_cost_usd,
                });
            }
        }
        for task_runs in runs.values_mut() {
            task_runs.sort_by_key(|run| run.start);
        }
    }
    let no_runs: &[Run] = &[];

    let mut history = Vec::new();
    let mut without_invocations = 0u64;
    for row in &history_rows {
        let used = usage(runs.get(&row.task_id).map_or(no_runs, Vec::as_slice), None);
        if used.invocations == 0 {
            without_invocations += 1;
            continue;
        }
        history.push(Delivery {
            complexity: row.complexity.clone(),
            crew: tasks.get(&row.task_id).and_then(|task| task.crew.clone()),
            hours: used.hours,
            tokens: used.tokens,
            usd: used.usd,
        });
    }

    let pools = complexity_pools(runtime);
    let mut queued: Vec<&Task> = tasks
        .values()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Backlog | TaskStatus::InProgress | TaskStatus::Proposed
            )
        })
        .collect();
    // In-progress work holds its slots first, then the backlog in dispatch
    // order, then proposed work (counted only if approved).
    queued.sort_by(|left, right| {
        let stage = |task: &Task| match task.status {
            TaskStatus::InProgress => 0,
            TaskStatus::Backlog => 1,
            _ => 2,
        };
        stage(left)
            .cmp(&stage(right))
            .then(priority_rank(left.priority).cmp(&priority_rank(right.priority)))
            .then(left.created_at.cmp(&right.created_at))
            .then(left.id.to_string().cmp(&right.id.to_string()))
    });
    let items: Vec<Item> = queued
        .iter()
        .map(|task| {
            let id = task.id.to_string();
            let complexity = complexity_bucket(task.complexity.map(|c| c.as_str())).to_string();
            let crews = match &task.crew {
                Some(crew) => BTreeSet::from([crew.clone()]),
                None => pools.get(&complexity).cloned().unwrap_or_default(),
            };
            let in_progress = task.status == TaskStatus::InProgress;
            let spent = if in_progress {
                let used = usage(runs.get(&id).map_or(no_runs, Vec::as_slice), None);
                Spent {
                    hours: used.hours,
                    tokens: used.tokens.unwrap_or(0.0),
                    usd: used.usd.unwrap_or(0.0),
                }
            } else {
                Spent::default()
            };
            Item {
                task_id: id,
                complexity,
                crews,
                in_progress,
                proposed: task.status == TaskStatus::Proposed,
                spent,
            }
        })
        .collect();
    let resolved: Vec<Resolved> = items
        .iter()
        .map(|item| model::resolve(item, &history))
        .collect();

    let (concurrency, concurrency_source) = match options.concurrency {
        Some(requested) => (u64::from(requested), "requested"),
        None => runtime.drain_concurrency_ceiling()?,
    };
    let slots = usize::try_from(concurrency).unwrap_or(1).max(1);

    let mut arrivals_by_source: BTreeMap<&str, u64> =
        [("auto_task", 0), ("sweep", 0), ("filing", 0)].into();
    for task in tasks
        .values()
        .filter(|task| task.created_at >= arrival_start)
    {
        *arrivals_by_source.entry(arrival_source(task)).or_default() += 1;
    }
    let arrived: u64 = arrivals_by_source.values().sum();
    let source_count = |source: &str| arrivals_by_source.get(source).copied().unwrap_or(0);
    let per_day = |count: u64| (count as f64 / ARRIVAL_DAYS as f64 * 100.0).round() / 100.0;
    let done_recently = done_at.values().filter(|at| **at >= arrival_start).count() as u64;
    let arrival_rate = arrived as f64 / ARRIVAL_DAYS as f64;
    let arrivals = options.arrivals.then_some(Arrivals {
        per_day: arrival_rate,
        history: &history,
    });

    let forecast = model::simulate(&items, &resolved, &history, slots, arrivals.as_ref());

    let coverage = |proposed: bool| {
        let mut counts = BTreeMap::from([
            ("tasks", 0u64),
            ("unassessed", 0),
            ("thin_cell_fallbacks", 0),
            ("fallback_complexity", 0),
            ("fallback_workspace", 0),
            ("uncovered_model", 0),
            ("uncosted", 0),
            ("no_history", 0),
        ]);
        let mut bump = |key: &'static str| *counts.entry(key).or_default() += 1;
        for (item, resolved) in items.iter().zip(&resolved) {
            if item.proposed != proposed {
                continue;
            }
            bump("tasks");
            if item.complexity == UNSET_BUCKET {
                bump("unassessed");
            }
            match resolved.basis {
                Basis::Cell => {}
                Basis::Complexity => {
                    bump("thin_cell_fallbacks");
                    bump("fallback_complexity");
                }
                Basis::Workspace => {
                    if item.complexity != UNSET_BUCKET {
                        bump("thin_cell_fallbacks");
                    }
                    bump("fallback_workspace");
                }
                Basis::None => bump("no_history"),
            }
            if resolved.uncovered_model() {
                bump("uncovered_model");
            }
            if resolved.uncosted() {
                bump("uncosted");
            }
        }
        counts
    };
    let status_count =
        |status: TaskStatus| tasks.values().filter(|task| task.status == status).count();
    let inputs = json!({
        "lookback_days": options.lookback_days,
        "lookback_start": lookback_start.to_rfc3339(),
        "concurrency": concurrency,
        "concurrency_source": concurrency_source,
        "auto_concurrency_mode": runtime.auto_concurrency_settings().mode,
        "arrivals": options.arrivals,
        "iterations": model::ITERATIONS,
        "min_cell_samples": model::MIN_CELL_SAMPLES,
        "gap_bridge_hours": GAP_BRIDGE_HOURS,
        "history_deliveries": history.len(),
        "history_without_invocations": without_invocations,
    });

    let mut stored_now = None;
    let backlog_count = items.iter().filter(|item| !item.proposed).count();
    if options.canonical() && backlog_count > 0 && !history.is_empty() {
        let stored_tasks: Vec<Value> = items
            .iter()
            .zip(&resolved)
            .filter(|(item, _)| !item.proposed)
            .map(|(item, resolved)| {
                json!({
                    "id": item.task_id,
                    "complexity": item.complexity,
                    "in_progress": item.in_progress,
                    "basis": resolved.basis.key(),
                    "cell_samples": resolved.cell_samples,
                    "forecast": resolved.marginals(&history, item.spent),
                })
            })
            .collect();
        let row = StoredBacklogForecast {
            workspace_id: String::new(),
            forecast_at: now,
            model_version: model::MODEL_VERSION,
            inputs_json: inputs.to_string(),
            result_json: forecast.backlog.to_string(),
            tasks_json: Value::Array(stored_tasks).to_string(),
        };
        match runtime.record_backlog_forecast(row, Duration::minutes(STORE_INTERVAL_MINUTES)) {
            Ok(written) => stored_now = Some(written),
            Err(error) => tracing::warn!(%error, "backlog forecast: not stored"),
        }
    }
    let stored = if stored_now == Some(true) {
        runtime.backlog_forecasts(SCORED_FORECASTS)?
    } else {
        stored
    };
    let calibration = scoring::score(&stored, &tasks, &done_at, &runs);

    Ok(json!({
        "model_version": model::MODEL_VERSION,
        "generated_at": now.to_rfc3339(),
        "inputs": inputs,
        "assumptions": {
            "arrivals": if options.arrivals { "recent_rate" } else { "none" },
            "excluded_statuses": ["review", "blocked", "someday"],
            "arrival_missing_metrics": "propagate_null",
        },
        "backlog": {
            "backlog": status_count(TaskStatus::Backlog),
            "in_progress": status_count(TaskStatus::InProgress),
            "proposed": status_count(TaskStatus::Proposed),
            "review": status_count(TaskStatus::Review),
            "blocked": status_count(TaskStatus::Blocked),
        },
        "forecast": forecast.backlog,
        "if_approved": forecast.if_approved,
        "coverage": coverage(false),
        "proposed_coverage": coverage(true),
        "arrivals": {
            "window_days": ARRIVAL_DAYS,
            // Shared by backlog and if-approved arrivals, even when off.
            // Initial-task coverage above describes a different sampling pool.
            "coverage": {
                "deliveries": history.len(),
                "missing_tokens": history.iter().filter(|d| d.tokens.is_none()).count(),
                "missing_usd": history.iter().filter(|d| d.usd.is_none()).count(),
            },
            "per_day": {
                "total": per_day(arrived),
                "auto_task": per_day(source_count("auto_task")),
                "sweep": per_day(source_count("sweep")),
                "filing": per_day(source_count("filing")),
            },
            "done_per_day": per_day(done_recently),
        },
        "stored": {
            "canonical": options.canonical(),
            "recorded": stored_now,
            "latest_at": stored.first().map(|forecast| forecast.forecast_at.to_rfc3339()),
            "min_interval_minutes": STORE_INTERVAL_MINUTES,
        },
        "calibration": calibration,
    }))
}
