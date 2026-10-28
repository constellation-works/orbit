use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{
    Task, TaskReferenceIndex, TaskRelationType, TaskStatus, auto_task_template, complexity_bucket,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;

use super::cohorts::ForecastCohorts;
use super::history::LoadedForecastHistory;
use super::model::{Item, Resolved, Spent};
use super::options::Options;
use super::usage::ForecastRun;
use super::{dependencies, model, outcomes, resource_fit, rework, usage};

pub(super) struct ForecastSimulationInputs {
    pub(super) items: Vec<Item>,
    pub(super) resolved: Vec<Resolved>,
    pub(super) age_unknown: BTreeSet<String>,
    pub(super) rework: rework::RegressionRework,
    pub(super) concurrency: u64,
    pub(super) concurrency_source: &'static str,
    pub(super) slots: usize,
    pub(super) arrivals_by_source: BTreeMap<&'static str, u64>,
    pub(super) arrived: u64,
    pub(super) adjusted_arrivals: BTreeMap<&'static str, u64>,
    pub(super) arrival_rates: BTreeMap<&'static str, f64>,
    pub(super) done_recently: u64,
    pub(super) plan: dependencies::DependencyPlan,
    pub(super) resource_fit_report: Value,
    pub(super) admission: crate::application::admission::forecast::ForecastAdmission,
}
/// Days the arrival rate is measured over.
pub(super) const ARRIVAL_DAYS: i64 = 7;
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
pub(super) fn arrival_source(task: &Task) -> &'static str {
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

pub(super) fn assemble_forecast_simulation(
    runtime: &OrbitRuntime,
    options: &Options,
    loaded: &LoadedForecastHistory,
    cohorts: &ForecastCohorts,
    now: DateTime<Utc>,
) -> Result<ForecastSimulationInputs, OrbitError> {
    let tasks = &loaded.tasks;
    let done_at = &loaded.done_at;
    let task_episodes = &loaded.task_episodes;
    let lookback_start = loaded.lookback_start;
    let arrival_start = loaded.arrival_start;
    let runs = &cohorts.runs;
    let reviews = &cohorts.reviews;
    let history = &cohorts.history;
    let episodes = &cohorts.episodes;
    let waits = &cohorts.waits;
    let no_runs: &[ForecastRun] = &[];
    let task_runs = |id: &str| runs.get(id).map_or(no_runs, Vec::as_slice);
    let last_setback = |id: &str, until: Option<DateTime<Utc>>| {
        task_episodes
            .get(id)
            .and_then(|episodes| episodes.last_setback(until))
    };
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
                let used = usage::forecast_usage(task_runs(&id), last_setback(&id, None));
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
        .map(|item| model::resolve_forecast_item(item, history, episodes, waits))
        .collect();

    let risk_model = runtime.task_riskiness_model()?;
    let mut rework = rework::RegressionRework {
        donors: rework::collect_forecast_donors(tasks, history, episodes, waits),
        bundles: Vec::new(),
        pools: Vec::new(),
        pool_basis: Vec::new(),
        probabilities: Vec::new(),
        prior_basis: Vec::new(),
        risks: Vec::new(),
        known_origins: BTreeMap::new(),
        missing_fix_history: 0,
    };
    for task in &queued {
        let risk = risk_model.estimate(task);
        let (probability, basis) = match risk.probability {
            Some(p) => (Some(p), "estimate"),
            None => risk_model.forecast_regression_prior(task),
        };
        rework.probabilities.push(probability);
        rework.prior_basis.push(basis);
        rework.risks.push(json!(risk));
    }
    rework.missing_fix_history = tasks
        .values()
        .filter(|task| {
            done_at
                .get(task.id.as_str())
                .is_some_and(|at| *at >= lookback_start)
                && task
                    .relations
                    .iter()
                    .any(|link| link.relation_type == TaskRelationType::RegressionFrom)
                && !rework.donors.iter().any(|donor| donor.task.id == task.id)
        })
        .count();
    rework.build_regression_bundles(tasks, done_at, &items);

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
    let mut adjusted_arrivals = arrivals_by_source.clone();
    for task in tasks.values().filter(|task| {
        task.created_at >= arrival_start
            && task
                .relations
                .iter()
                .any(|link| link.relation_type == TaskRelationType::RegressionFrom)
    }) {
        if let Some(count) = adjusted_arrivals.get_mut(arrival_source(task)) {
            *count = count.saturating_sub(1);
        }
    }
    let arrival_rates: BTreeMap<&'static str, f64> = adjusted_arrivals
        .iter()
        .map(|(source, count)| (*source, *count as f64 / ARRIVAL_DAYS as f64))
        .collect();
    let done_recently = done_at.values().filter(|at| **at >= arrival_start).count() as u64;

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
            .chain(rework.donors.iter().map(|donor| Task {
                status: TaskStatus::Backlog,
                ..donor.task.clone()
            }))
            .collect(),
        &workspace,
        now,
    );
    if let Some(evidence) = resource_fit.evidence {
        admission.fit_resources(evidence);
    }
    Ok(ForecastSimulationInputs {
        items,
        resolved,
        age_unknown,
        rework,
        concurrency,
        concurrency_source,
        slots,
        arrivals_by_source,
        arrived,
        adjusted_arrivals,
        arrival_rates,
        done_recently,
        plan,
        resource_fit_report: resource_fit.report,
        admission,
    })
}
