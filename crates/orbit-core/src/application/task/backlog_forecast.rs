//! Backlog forecast for Analytics › Forecast [ORB-18385]: how long the
//! dispatchable backlog takes to drain, and what it costs in USD and
//! normalized tokens, as p50 and p90 from resampled delivery history, plus
//! the calibration of earlier stored forecasts.
//!
//! The method is documented in `docs/design/backlog-forecast/1_scope.md`. In short:
//!
//! - A delivering attempt draws one completed delivery: a task that reached
//!   `done` inside the lookback, with every managed invocation linked to it
//!   since its last attempt that did not deliver (recoveries, auto-fixes
//!   and reworks included). Its hours are the union of those invocations'
//!   run intervals, bridging gaps shorter than [`GAP_BRIDGE_HOURS`]; its
//!   tokens and USD are their normalized token and provider cost sums.
//! - Each attempt first draws an admission episode from status history,
//!   delivered or not, and a blocked one draws its return from a
//!   competing-risk return curve [ORB-18688] (`outcomes.rs`). Blocked tasks
//!   join the cohort waiting for that return.
//! - Each task draws from its complexity × crew-pool × review-mode cell.
//!   Thin cells drop crew, then complexity, preserving mode at five samples;
//!   only then use the mode-pooled cell, complexity and workspace history.
//! - The drain is simulated on the drain's leaf-run ceiling
//!   ([`OrbitRuntime::drain_concurrency_ceiling`]), with no new arrivals
//!   unless `arrivals=on`. Each iteration admits work as the drain would
//!   [ORB-18686]: in its dispatch order, once in-forecast dependencies have
//!   finished, never beside overlapping locks, with unknown footprints run
//!   alone and surface reservations honoured (`schedule.rs`, through the
//!   admission module's forecast adapter). A task whose dependency the
//!   forecast cannot schedule leaves the full-backlog totals unknown and is
//!   reported with a schedulable subset (`dependencies.rs`).
//! - Where the drain enforces gap fit, each simulated pass also fits
//!   candidates against the host's CPU and memory, from this host's profiles,
//!   size and baseline [ORB-18689] (`resource_fit.rs`). The ceiling stays the
//!   upper bound; gap fit only holds work below it.
//!
//! The canonical forecast (default lookback, the live ceiling, no arrivals)
//! is stored at most once per workspace per [`STORE_INTERVAL_MINUTES`], and
//! the owner clock records it even without reads; every request scores the
//! newest stored forecasts.

mod dependencies;
mod model;
mod outcomes;
mod resource_fit;
mod review_modes;
mod schedule;
mod scoring;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::OrbitRuntime;
use crate::runtime::host_resource::profile::ProfileSource;
use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{BacklogForecastStoreBackend as _, StoredBacklogForecast};
use orbit_types::task::{
    Task, TaskHistoryEntry, TaskReferenceIndex, TaskStatus, UNSET_BUCKET, auto_task_template,
    complexity_bucket,
};
use serde_json::{Value, json};

use model::{Arrivals, Basis, Delivery, Item, Resolved, Spent};
use outcomes::{AwayWait, Episode, EpisodeOutcome, RETURN_HORIZON_HOURS, TaskEpisodes};
use review_modes::{ReviewHistory, ReviewMode};

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

fn parse_options(
    lookback_days: Option<i64>,
    concurrency: Option<u32>,
    arrivals: Option<&str>,
) -> Result<Options, OrbitError> {
    let invalid = |message: String| OrbitError::InvalidInput(message);
    let lookback_days = lookback_days.unwrap_or(DEFAULT_LOOKBACK_DAYS);
    if !(MIN_LOOKBACK_DAYS..=MAX_LOOKBACK_DAYS).contains(&lookback_days) {
        return Err(invalid(format!(
            "lookback_days must be between {MIN_LOOKBACK_DAYS} and {MAX_LOOKBACK_DAYS}"
        )));
    }
    if let Some(concurrency) = concurrency
        && !(1..=MAX_CONCURRENCY).contains(&concurrency)
    {
        return Err(invalid(format!(
            "concurrency must be between 1 and {MAX_CONCURRENCY}"
        )));
    }
    let arrivals = match arrivals.unwrap_or("off") {
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
        concurrency,
        arrivals,
    })
}

/// One managed invocation linked to a task: when it ran and what it used.
pub(super) struct ForecastRun {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    tokens: Option<f64>,
    usd: Option<f64>,
}

/// What a task's invocations used, from some time on.
pub(super) struct ForecastUsage {
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
pub(super) fn forecast_usage(runs: &[ForecastRun], after: Option<DateTime<Utc>>) -> ForecastUsage {
    let bridge = Duration::seconds((GAP_BRIDGE_HOURS * 3600.0) as i64);
    let mut used = ForecastUsage {
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

/// What the runs overlapping `from`..`to` used: one attempt's tokens and USD.
fn window_spend(
    runs: &[ForecastRun],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> (Option<f64>, Option<f64>) {
    runs.iter()
        .filter(|run| run.end > from && run.start < to)
        .fold((Some(0.0), Some(0.0)), |(tokens, usd), run| {
            (
                tokens.zip(run.tokens).map(|(sum, one)| sum + one),
                usd.zip(run.usd).map(|(sum, one)| sum + one),
            )
        })
}

/// Read `id`'s status history once per forecast; `None` when unreadable.
fn cached_history<'a>(
    runtime: &OrbitRuntime,
    cache: &'a mut HashMap<String, Option<Vec<TaskHistoryEntry>>>,
    id: &str,
) -> Option<&'a [TaskHistoryEntry]> {
    cache
        .entry(id.to_string())
        .or_insert_with(|| match runtime.get_task_history(id) {
            Ok(history) => Some(history),
            Err(error) => {
                tracing::warn!(task_id = %id, %error, "backlog forecast: task history unreadable");
                None
            }
        })
        .as_deref()
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

fn calculate_backlog_forecast(
    runtime: &OrbitRuntime,
    options: &Options,
    now: DateTime<Utc>,
) -> Result<Value, OrbitError> {
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
    let mut histories: HashMap<String, Option<Vec<TaskHistoryEntry>>> = HashMap::new();
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
        let Some(task_history) = cached_history(runtime, &mut histories, &row.task_id) else {
            continue;
        };
        let completed_at = task_history
            .iter()
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

    // Attempts and returns come from status history. Returns are observed
    // from 90 days before the lookback, so a recent lookback need not prove
    // a 90-day tail on its own. `updated_at` only narrows the candidates: a
    // transition cannot be newer than its task's last update. Running and
    // blocked tasks are always read, for their elapsed age.
    let return_start = lookback_start - Duration::days(outcomes::RETURN_WINDOW_DAYS);
    let mut task_episodes: HashMap<String, TaskEpisodes> = HashMap::new();
    let mut episode_ids: Vec<&String> = tasks
        .iter()
        .filter(|(_, task)| {
            task.updated_at >= return_start
                || matches!(task.status, TaskStatus::InProgress | TaskStatus::Blocked)
        })
        .map(|(id, _)| id)
        .collect();
    episode_ids.sort();
    for id in episode_ids {
        if let Some(history) = cached_history(runtime, &mut histories, id) {
            task_episodes.insert(id.clone(), TaskEpisodes::read(history));
        }
    }
    drop(histories);
    let attempted = task_episodes.iter().any(|(_, episodes)| {
        episodes
            .settled
            .iter()
            .any(|(admitted, _, _)| *admitted >= lookback_start)
    });

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
        .chain((!history_rows.is_empty() || attempted).then_some(lookback_start))
        .min()
        .map(|oldest| oldest - SPEND_READ_SLACK);
    let mut runs: HashMap<String, Vec<ForecastRun>> = HashMap::new();
    let spends = if spend_since.is_some() {
        runtime.invocation_spend(spend_since)?
    } else {
        Vec::new()
    };
    let review_task_ids = history_rows
        .iter()
        .map(|row| row.task_id.as_str())
        .chain(task_episodes.keys().map(String::as_str))
        .collect();
    let reviews = ReviewHistory::collect(runtime, &spends, &review_task_ids)?;
    if spend_since.is_some() {
        for spend in spends {
            let end = spend.ts;
            let start = end - Duration::milliseconds(i64::try_from(spend.duration_ms).unwrap_or(0));
            for task_id in &spend.task_ids {
                runs.entry(task_id.clone()).or_default().push(ForecastRun {
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
    let no_runs: &[ForecastRun] = &[];

    let mut history = Vec::new();
    let mut without_invocations = 0u64;
    let task_runs = |id: &str| runs.get(id).map_or(no_runs, Vec::as_slice);
    let last_setback = |id: &str, until: Option<DateTime<Utc>>| {
        task_episodes
            .get(id)
            .and_then(|episodes| episodes.last_setback(until))
    };
    for row in &history_rows {
        // Attempts that did not deliver are sampled as episodes, so the
        // delivery counts only what followed the last of them.
        let since = last_setback(&row.task_id, done_at.get(&row.task_id).copied());
        let used = forecast_usage(task_runs(&row.task_id), since);
        if used.invocations == 0 {
            without_invocations += 1;
            continue;
        }
        history.push(Delivery {
            complexity: row.complexity.clone(),
            crew: tasks.get(&row.task_id).and_then(|task| task.crew.clone()),
            review_mode: reviews.delivery_mode(&row.task_id),
            hours: used.hours,
            tokens: used.tokens,
            usd: used.usd,
        });
    }

    let sample_cell = |id: &str| {
        let task = tasks.get(id);
        (
            complexity_bucket(task.and_then(|task| task.complexity.map(|c| c.as_str())))
                .to_string(),
            task.and_then(|task| task.crew.clone()),
            reviews.delivery_mode(id),
        )
    };
    let mut episodes: Vec<Episode> = Vec::new();
    let mut waits: Vec<AwayWait> = Vec::new();
    let mut live_episodes = 0u64;
    let mut episode_ids: Vec<&String> = task_episodes.keys().collect();
    episode_ids.sort();
    for id in episode_ids {
        let recorded = &task_episodes[id];
        let (complexity, crew, review_mode) = sample_cell(id);
        if recorded
            .live
            .is_some_and(|admitted| admitted >= lookback_start)
        {
            live_episodes += 1;
        }
        for (admitted, settled, outcome) in &recorded.settled {
            if *admitted < lookback_start {
                continue;
            }
            let (tokens, usd) = if *outcome == EpisodeOutcome::Delivered {
                (None, None)
            } else {
                window_spend(task_runs(id), *admitted, *settled)
            };
            episodes.push(Episode {
                complexity: complexity.clone(),
                crew: crew.clone(),
                review_mode,
                outcome: *outcome,
                hold_hours: outcomes::hours_between(*admitted, *settled),
                tokens,
                usd,
            });
        }
        for (left, follow) in &recorded.away {
            if *left < return_start {
                continue;
            }
            let (hours, event) = match follow {
                Some((at, event)) => (outcomes::hours_between(*left, *at), Some(*event)),
                None => (outcomes::hours_between(*left, now), None),
            };
            // Past the horizon a wait is censored there: no return within it.
            let (hours, event) = if hours > RETURN_HORIZON_HOURS {
                (RETURN_HORIZON_HOURS, None)
            } else {
                (hours, event)
            };
            waits.push(AwayWait {
                complexity: complexity.clone(),
                crew: crew.clone(),
                review_mode,
                hours,
                event,
            });
        }
    }

    let pools = complexity_pools(runtime);
    let mut queued: Vec<&Task> = tasks
        .values()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Backlog
                    | TaskStatus::InProgress
                    | TaskStatus::Proposed
                    | TaskStatus::Blocked
            )
        })
        .collect();
    // A stable item order; the simulation admits in the drain's own
    // dispatch order, interleaving proposed work when approved.
    queued.sort_by(|left, right| left.id.cmp(&right.id));
    let mut age_unknown: BTreeSet<String> = BTreeSet::new();
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
            let blocked = task.status == TaskStatus::Blocked;
            let recorded = task_episodes.get(&id);
            // Only the running attempt's use is subtracted; a blocked task's
            // earlier use is sunk.
            let spent = if in_progress {
                let used = forecast_usage(task_runs(&id), last_setback(&id, None));
                Spent {
                    hours: used.hours,
                    tokens: used.tokens.unwrap_or(0.0),
                    usd: used.usd.unwrap_or(0.0),
                }
            } else {
                Spent::default()
            };
            let since = if in_progress {
                recorded.and_then(|recorded| recorded.live)
            } else if blocked {
                recorded.and_then(|recorded| recorded.blocked_since)
            } else {
                None
            };
            if (in_progress || blocked) && since.is_none() {
                age_unknown.insert(id.clone());
            }
            let (review_mode, review_mode_source) = reviews.assumed_mode(runtime, task)?;
            Ok(Item {
                task_id: id,
                title: task.title.clone(),
                complexity,
                crews,
                review_mode,
                review_mode_source,
                in_progress,
                proposed: task.status == TaskStatus::Proposed,
                blocked,
                age_hours: since.map_or(0.0, |since| outcomes::hours_between(since, now)),
                spent,
            })
        })
        .collect::<Result<_, OrbitError>>()?;
    let resolved: Vec<Resolved> = items
        .iter()
        .map(|item| model::resolve_forecast_item(item, &history, &episodes, &waits))
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

    let status_by_id = runtime.dependency_status_index(queued.iter().copied())?;
    let reference_index = TaskReferenceIndex::from_status_index(&status_by_id);
    // A blocked task with no return evidence has no start time to simulate.
    let no_return_support: Vec<Option<&'static str>> = items
        .iter()
        .zip(&resolved)
        .map(|(item, resolved)| {
            (item.blocked && resolved.returns.support == 0).then_some("return_unsupported")
        })
        .collect();
    let plan = dependencies::plan_forecast_dependencies(
        &queued,
        &status_by_id,
        &reference_index,
        &no_return_support,
    );
    let workspace: BTreeMap<String, Task> = tasks
        .iter()
        .map(|(id, task)| (id.clone(), task.clone()))
        .collect();
    let resource_fit = resource_fit::resource_fit_inputs(runtime, concurrency_source, now)?;
    // A blocked task re-enters the queue as backlog, with its own priority
    // and footprint.
    let mut admission = runtime.forecast_admission(
        queued
            .iter()
            .map(|task| match task.status {
                TaskStatus::Blocked => Task {
                    status: TaskStatus::Backlog,
                    ..(*task).clone()
                },
                _ => (*task).clone(),
            })
            .collect(),
        &workspace,
        now,
    );
    if let Some(evidence) = resource_fit.evidence {
        admission.fit_resources(evidence);
    }
    let forecast = model::simulate_backlog_forecast(
        &items,
        &resolved,
        &history,
        &episodes,
        &admission,
        &plan,
        slots,
        arrivals.as_ref(),
    );

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
            ("dependency_unforecastable", 0),
            ("unverifiable_dependencies", 0),
            ("review_lock_overlaps", 0),
            ("review_mode_fallbacks", 0),
            ("review_mode_unknown", 0),
            ("blocked", 0),
            ("outcome_fallbacks", 0),
            ("outcome_no_history", 0),
            ("return_unsupported", 0),
            ("return_tail_unsupported", 0),
            ("phase_age_unknown", 0),
            ("resource_profile_fallbacks", 0),
            ("resource_profile_missing", 0),
            (
                "history_review_mode_unknown",
                history
                    .iter()
                    .filter(|delivery| delivery.review_mode == ReviewMode::Unknown)
                    .count() as u64,
            ),
        ]);
        let reasons = if proposed {
            &plan.approved
        } else {
            &plan.backlog
        };
        let mut unforecastable: BTreeMap<&str, u64> = BTreeMap::new();
        let mut bump = |key: &'static str| *counts.entry(key).or_default() += 1;
        for (index, (item, resolved)) in items.iter().zip(&resolved).enumerate() {
            if item.proposed != proposed {
                continue;
            }
            bump("tasks");
            match reasons[index] {
                Some("return_unsupported") => bump("return_unsupported"),
                Some(reason) => {
                    bump("dependency_unforecastable");
                    *unforecastable.entry(reason).or_default() += 1;
                }
                None => {}
            }
            if item.blocked {
                bump("blocked");
            }
            match resolved.outcome_basis {
                Basis::None => bump("outcome_no_history"),
                Basis::Cell => {}
                _ => bump("outcome_fallbacks"),
            }
            let age = if item.blocked { item.age_hours } else { 0.0 };
            if (item.blocked || resolved.can_block(&episodes))
                && resolved.returns.support > 0
                && resolved.returns.unsupported_at(age)
            {
                bump("return_tail_unsupported");
            }
            if age_unknown.contains(&item.task_id) {
                bump("phase_age_unknown");
            }
            if admission.overlaps_review_lock(index) {
                bump("review_lock_overlaps");
            }
            match admission.profile_source(index) {
                Some(ProfileSource::None) => bump("resource_profile_missing"),
                Some(ProfileSource::Exact) | None => {}
                Some(_) => bump("resource_profile_fallbacks"),
            }
            for _ in 0..plan.unverifiable[index] {
                bump("unverifiable_dependencies");
            }
            if item.complexity == UNSET_BUCKET {
                bump("unassessed");
            }
            if resolved.review_mode_fallback {
                bump("review_mode_fallbacks");
            }
            if item.review_mode == ReviewMode::Unknown {
                bump("review_mode_unknown");
            }
            match resolved.basis {
                Basis::Cell => {}
                Basis::ModePooledCell => bump("thin_cell_fallbacks"),
                Basis::Complexity | Basis::ReviewComplexity => {
                    bump("thin_cell_fallbacks");
                    bump("fallback_complexity");
                }
                Basis::Workspace | Basis::ReviewWorkspace => {
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
        let mut coverage: serde_json::Map<String, Value> = counts
            .into_iter()
            .map(|(key, count)| (key.to_string(), json!(count)))
            .collect();
        coverage.insert(
            "dependency_unforecastable_reasons".into(),
            json!(unforecastable),
        );
        Value::Object(coverage)
    };
    let status_count =
        |status: TaskStatus| tasks.values().filter(|task| task.status == status).count();
    let inputs = json!({
        "lookback_days": options.lookback_days,
        "lookback_start": lookback_start.to_rfc3339(),
        "concurrency": concurrency,
        "concurrency_source": concurrency_source,
        "auto_concurrency_mode": runtime.auto_concurrency_settings().mode,
        "resource_fit": resource_fit.report,
        "arrivals": options.arrivals,
        "iterations": model::ITERATIONS,
        "horizon_hours": schedule::HORIZON_HOURS,
        "admission_factors": model::ADMISSION_FACTORS
            .iter()
            .map(|(factor, revision)| (factor.to_string(), json!(revision)))
            .collect::<serde_json::Map<String, Value>>(),
        "min_cell_samples": model::MIN_CELL_SAMPLES,
        "gap_bridge_hours": GAP_BRIDGE_HOURS,
        "history_deliveries": history.len(),
        "history_without_invocations": without_invocations,
        "history_review_modes": history.iter().fold(BTreeMap::<String, u64>::new(), |mut counts, delivery| {
            *counts.entry(delivery.review_mode.key().into()).or_default() += 1;
            counts
        }),
        "outcomes": {
            "episodes": episodes.iter().fold(BTreeMap::<&str, u64>::new(), |mut counts, episode| {
                *counts.entry(episode.outcome.key()).or_default() += 1;
                counts
            }),
            "live_episodes": live_episodes,
            "return_window_days": options.lookback_days + outcomes::RETURN_WINDOW_DAYS,
            "return_horizon_hours": RETURN_HORIZON_HOURS,
            "returns": {
                "waits": waits.len(),
                "returned": waits.iter().filter(|wait| wait.event == Some(outcomes::ReturnEvent::Returned)).count(),
                "resolved": waits.iter().filter(|wait| wait.event == Some(outcomes::ReturnEvent::Resolved)).count(),
                "absorbed": waits.iter().filter(|wait| wait.event == Some(outcomes::ReturnEvent::Absorbed)).count(),
                "censored": waits.iter().filter(|wait| wait.event.is_none()).count(),
            },
        },
    });

    let mut stored_now = None;
    let backlog_count = items.iter().filter(|item| !item.proposed).count();
    if options.canonical() && backlog_count > 0 && !history.is_empty() {
        let predictions: HashMap<&str, &Value> = forecast.backlog["task_predictions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| Some((row["id"].as_str()?, row)))
            .collect();
        let stored_tasks: Vec<Value> = items
            .iter()
            .zip(&resolved)
            .enumerate()
            .filter(|(_, (item, _))| !item.proposed)
            .map(|(index, (item, resolved))| {
                let prediction = predictions.get(item.task_id.as_str());
                let field = |key: &str| prediction.map_or(Value::Null, |row| row[key].clone());
                json!({
                    "id": item.task_id,
                    "complexity": item.complexity,
                    "in_progress": item.in_progress,
                    "status": if item.in_progress {
                        "in-progress"
                    } else if item.blocked {
                        "blocked"
                    } else {
                        "backlog"
                    },
                    "basis": resolved.basis.key(),
                    "outcome_basis": resolved.outcome_basis.key(),
                    "setback_probability": resolved.setback_probability(&episodes),
                    "return_basis": resolved.return_basis.key(),
                    "return_probability": resolved.returns.return_probability(),
                    "cell_samples": resolved.cell_samples,
                    "review_mode": item.review_mode,
                    "review_mode_source": item.review_mode_source,
                    "review_mode_fallback": resolved.review_mode_fallback,
                    "forecast": resolved.marginals(&history, item.spent),
                    "dependencies": plan.edges[index]
                        .iter()
                        .map(|dep| items[*dep].task_id.as_str())
                        .collect::<Vec<_>>(),
                    "footprint": admission.surface_hash(index),
                    "profile_source": admission.profile_source(index).map(|source| source.as_str()),
                    "unforecastable": plan.backlog[index],
                    "predicted_start_hours": field("predicted_start_hours"),
                    "predicted_start_rank": field("predicted_start_rank"),
                    "completion_probability": field("completion_probability"),
                })
            })
            .collect();
        // Per-task predictions are stored once, with each task above.
        let mut result = forecast.backlog.clone();
        if let Some(result) = result.as_object_mut() {
            result.remove("task_predictions");
        }
        let row = StoredBacklogForecast {
            workspace_id: String::new(),
            forecast_at: now,
            model_version: model::MODEL_VERSION,
            inputs_json: inputs.to_string(),
            result_json: result.to_string(),
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
    let calibration =
        scoring::score_backlog_forecasts(&stored, &tasks, &done_at, &runs, model::MODEL_VERSION);

    Ok(json!({
        "model_version": model::MODEL_VERSION,
        "generated_at": now.to_rfc3339(),
        "inputs": inputs,
        "assumptions": {
            "arrivals": if options.arrivals { "recent_rate" } else { "none" },
            "excluded_statuses": ["review", "someday"],
            // A blocked task waits for a sampled return, conditioned on how
            // long it has been blocked; attempts sample episode outcomes.
            "blocked_tasks": "age_conditioned_return",
            "attempt_outcomes": "sampled_episodes",
            "arrival_missing_metrics": "propagate_null",
            // Admission factors the simulation does not model yet: review
            // locks are counted in coverage, arrivals take a slot but hold
            // no lock, and static eligibility gates are not replayed.
            "review_lock_holders": "not_modelled",
            "arrival_conflicts": "slot_only",
            // Gap fit judges initial tasks only where `inputs.resource_fit`
            // says it applied; arrivals take a slot without a profile.
            "resource_fit": "enforce_mode_gap_fit",
            "arrival_resources": "not_modelled",
            "static_eligibility": "not_modelled",
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

impl OrbitRuntime {
    /// Forecast this workspace's backlog and score earlier stored forecasts.
    ///
    /// Defaults use 30 days of history, the live drain ceiling and no arrivals.
    /// Canonical reads may record a snapshot when the hourly slot is free.
    /// Lookbacks outside 7–180 days, concurrency outside 1–64 and arrivals
    /// other than `on` or `off` are invalid inputs.
    pub fn backlog_forecast(
        &self,
        lookback_days: Option<i64>,
        concurrency: Option<u32>,
        arrivals: Option<&str>,
    ) -> Result<Value, OrbitError> {
        let options = parse_options(lookback_days, concurrency, arrivals)?;
        calculate_backlog_forecast(self, &options, Utc::now())
    }

    /// Fill the canonical hourly slot on the owner clock, without requiring
    /// an endpoint read. The transaction remains the authority if a read races
    /// this early check. Recent slots skip history reads and simulation.
    pub(crate) fn record_scheduled_backlog_forecast(
        &self,
        now: DateTime<Utc>,
    ) -> Result<(), OrbitError> {
        if self.worker_invocation().is_some() || self.coordination_write_owner().is_some() {
            return Ok(());
        }
        if self.backlog_forecasts(1)?.first().is_some_and(|latest| {
            latest.forecast_at > now - Duration::minutes(STORE_INTERVAL_MINUTES)
        }) {
            return Ok(());
        }
        // The canonical store keeps every non-proposed forecast item, so the
        // scheduled admission must match that base cohort: proposed-only
        // work stores nothing and stays skipped here, as before.
        if !self.list_task_metadata()?.iter().any(|task| {
            matches!(
                task.status,
                TaskStatus::Backlog | TaskStatus::InProgress | TaskStatus::Blocked
            )
        }) {
            return Ok(());
        }
        let options = parse_options(None, None, None)?;
        calculate_backlog_forecast(self, &options, now)?;
        Ok(())
    }

    /// Store `forecast` for this workspace unless one newer than
    /// `forecast.forecast_at - min_interval` is already stored. `true` when
    /// this call wrote it. The workspace id on `forecast` is replaced by this
    /// runtime's.
    pub fn record_backlog_forecast(
        &self,
        mut forecast: StoredBacklogForecast,
        min_interval: Duration,
    ) -> Result<bool, OrbitError> {
        forecast.workspace_id = self.workspace_id()?;
        self.sqlite_store()?
            .record_backlog_forecast(&forecast, min_interval)
    }

    /// This workspace's newest `limit` stored forecasts, newest first.
    pub fn backlog_forecasts(
        &self,
        limit: usize,
    ) -> Result<Vec<StoredBacklogForecast>, OrbitError> {
        self.sqlite_store()?
            .backlog_forecasts(&self.workspace_id()?, limit)
    }
}
