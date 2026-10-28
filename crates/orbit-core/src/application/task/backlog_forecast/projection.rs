use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::StoredBacklogForecast;
use orbit_types::task::{TaskStatus, UNSET_BUCKET};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::runtime::host_resource::profile::ProfileSource;

use super::assembly::{ARRIVAL_DAYS, ForecastSimulationInputs};
use super::cohorts::ForecastCohorts;
use super::history::LoadedForecastHistory;
use super::model::Basis;
use super::options::Options;
use super::outcomes::RETURN_HORIZON_HOURS;
use super::persistence::STORE_INTERVAL_MINUTES;
use super::review_modes::ReviewMode;
use super::usage::GAP_BRIDGE_HOURS;
use super::{assembly, model, outcomes, schedule, simulation};

pub(super) struct ForecastProjection<'a> {
    pub(super) runtime: &'a OrbitRuntime,
    pub(super) options: &'a Options,
    pub(super) loaded: &'a LoadedForecastHistory,
    pub(super) cohorts: &'a ForecastCohorts,
    pub(super) prepared: &'a ForecastSimulationInputs,
    pub(super) forecast: &'a simulation::Forecast,
    pub(super) now: DateTime<Utc>,
}

fn forecast_coverage(context: &ForecastProjection, proposed: bool) -> Value {
    let history = &context.cohorts.history;
    let episodes = &context.cohorts.episodes;
    let items = &context.prepared.items;
    let resolved = &context.prepared.resolved;
    let age_unknown = &context.prepared.age_unknown;
    let rework = &context.prepared.rework;
    let plan = &context.prepared.plan;
    let admission = &context.prepared.admission;

    let mut counts = BTreeMap::from([
        ("tasks", 0u64),
        ("risk_prior_fallbacks", 0),
        ("risk_unestimated", 0),
        ("regression_history_missing", 0),
        ("regression_history_thin", 0),
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
    for (index, (item, resolved)) in items.iter().zip(resolved).enumerate() {
        if item.proposed != proposed {
            continue;
        }
        bump("tasks");
        if rework.probabilities[index].is_none() {
            bump("risk_unestimated");
        } else if rework.prior_basis[index] != "estimate" {
            bump("risk_prior_fallbacks");
        }
        if rework.pools[index].is_empty() || rework.missing_fix_history > 0 {
            bump("regression_history_missing");
        } else if rework.pools[index].len() < model::MIN_CELL_SAMPLES {
            bump("regression_history_thin");
        }
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
        if (item.blocked || resolved.can_block(episodes))
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
            Some(ProfileSource::Crew | ProfileSource::Exact) | None => {}
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
}

pub(super) fn forecast_inputs(context: &ForecastProjection) -> Value {
    let runtime = context.runtime;
    let options = context.options;
    let lookback_start = context.loaded.lookback_start;
    let lookback_days = context.loaded.lookback_days;
    let min_history = context.loaded.min_history;
    let history = &context.cohorts.history;
    let without_invocations = context.cohorts.without_invocations;
    let episodes = &context.cohorts.episodes;
    let waits = &context.cohorts.waits;
    let live_episodes = context.cohorts.live_episodes;
    let rework = &context.prepared.rework;
    let concurrency = context.prepared.concurrency;
    let concurrency_source = context.prepared.concurrency_source;
    let resource_fit = &context.prepared.resource_fit_report;
    json!({
        "lookback_days": options.lookback_days,
        "lookback_start": lookback_start.to_rfc3339(),
        "history_tasks": options.history_tasks,
        "min_history_tasks": min_history,
        "concurrency": concurrency,
        "concurrency_source": concurrency_source,
        "auto_concurrency_mode": runtime.auto_concurrency_settings().mode,
        "resource_fit": resource_fit,
        "regression_rework": rework.regression_history_report(),
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
            "return_window_days": lookback_days + outcomes::RETURN_WINDOW_DAYS,
            "return_horizon_hours": RETURN_HORIZON_HOURS,
            "returns": {
                "waits": waits.len(),
                "returned": waits.iter().filter(|wait| wait.event == Some(outcomes::ReturnEvent::Returned)).count(),
                "resolved": waits.iter().filter(|wait| wait.event == Some(outcomes::ReturnEvent::Resolved)).count(),
                "absorbed": waits.iter().filter(|wait| wait.event == Some(outcomes::ReturnEvent::Absorbed)).count(),
                "censored": waits.iter().filter(|wait| wait.event.is_none()).count(),
            },
        },
    })
}

pub(super) fn forecast_result(
    context: &ForecastProjection,
    inputs: Value,
    stored_now: Option<bool>,
    stored: &[StoredBacklogForecast],
    calibration: Value,
    preparation_ms: f64,
    simulation_ms: f64,
) -> Result<Value, OrbitError> {
    let options = context.options;
    let now = context.now;
    let tasks = &context.loaded.tasks;
    let history = &context.cohorts.history;
    let rework = &context.prepared.rework;
    let arrivals_by_source = &context.prepared.arrivals_by_source;
    let arrived = context.prepared.arrived;
    let adjusted_arrivals = &context.prepared.adjusted_arrivals;
    let arrival_rates = &context.prepared.arrival_rates;
    let done_recently = context.prepared.done_recently;
    let forecast = context.forecast;
    let source_count = |source: &str| arrivals_by_source.get(source).copied().unwrap_or(0);
    let per_day = |count: u64| (count as f64 / ARRIVAL_DAYS as f64 * 100.0).round() / 100.0;
    let status_count =
        |status: TaskStatus| tasks.values().filter(|task| task.status == status).count();
    Ok(json!({
        "model_version": model::MODEL_VERSION,
        "generated_at": now.to_rfc3339(),
        "timing": {"preparation_ms": preparation_ms, "simulation_ms": simulation_ms},
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
            // locks are counted in coverage and static eligibility gates
            // are not replayed. Synthetic work uses historical donor locks.
            "review_lock_holders": "not_modelled",
            "arrival_conflicts": "historical_footprint_donors",
            "regression_rework": "one_generation_detection_bundles",
            "risk_prior": "supported_crew_complexity_else_workspace",
            // Gap fit judges initial tasks only where `inputs.resource_fit`
            // says it applied; synthetic work uses its donor profile.
            "resource_fit": "enforce_mode_gap_fit",
            "arrival_resources": "donor_profile",
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
        "coverage": forecast_coverage(context, false),
        "proposed_coverage": forecast_coverage(context, true),
        "arrivals": {
            "window_days": ARRIVAL_DAYS,
            // Shared by backlog and if-approved arrivals, even when off.
            // Initial-task coverage above describes a different sampling pool.
            "coverage": {
                "deliveries": history.len(),
                "conflict_donors": rework.donors.len(),
                "arrival_conflict_unknown": options.arrivals && adjusted_arrivals.values().sum::<u64>() > 0 && rework.donors.is_empty(),
                "source_class_fallbacks": arrival_rates.iter().filter(|(source, rate)| **rate > 0.0 && rework.donors.iter().filter(|d| assembly::arrival_source(&d.task) == **source).count() < model::MIN_CELL_SAMPLES).count(),
                "missing_tokens": history.iter().filter(|d| d.tokens.is_none()).count(),
                "missing_usd": history.iter().filter(|d| d.usd.is_none()).count(),
            },
            "per_day": {
                "total": per_day(arrived),
                "auto_task": per_day(source_count("auto_task")),
                "sweep": per_day(source_count("sweep")),
                "filing": per_day(source_count("filing")),
            },
            "adjusted_per_day": per_day(adjusted_arrivals.values().sum()),
            "excluded_regression_filings": arrived - adjusted_arrivals.values().sum::<u64>(),
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
