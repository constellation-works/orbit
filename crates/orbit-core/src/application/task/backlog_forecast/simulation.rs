//! Monte Carlo scenario aggregation, including synthetic follow-up work.

use super::dependencies::DependencyPlan;
use super::model::Delivery;
use super::model::{self, Draw, FORECAST_SEED, ITERATIONS, Item, Resolved, Rng, Script};
use super::outcomes::{Episode, MAX_ATTEMPTS};
use super::rework::{self, RegressionRework};
use super::schedule::{self, HORIZON_HOURS, Scenario, ScheduleState};
use crate::application::admission::forecast::{ForecastAdmission, ForecastWait};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn probability(count: u64, of: u64) -> Option<f64> {
    (of > 0).then(|| (count as f64 / of as f64 * 1000.0).round() / 1000.0)
}

/// The simulated totals and schedules for one scenario.
struct Outcome {
    drain: Vec<Option<f64>>,
    tokens: Vec<Option<f64>>,
    usd: Vec<Option<f64>>,
    critical_path: Vec<Option<f64>>,
    parallelism: Vec<Option<f64>>,
    /// Per [`ForecastWait::ALL`] reason: summed task wait hours.
    waits: [Vec<Option<f64>>; schedule::WAITS],
    /// Summed task-hours waiting to return after leaving the queue.
    return_waits: Vec<Option<f64>>,
    /// Over available iterations: attempts started that were blocked,
    /// failed or abandoned; tasks left without a return; tasks whose
    /// [`MAX_ATTEMPTS`]-th attempt started.
    setbacks: [u64; 3],
    no_return: u64,
    attempt_limited: u64,
    /// Iterations with hours to schedule; of those, the ones that drained
    /// and the ones that finished every scheduled initial task.
    available: u64,
    drained: u64,
    complete: u64,
    unfinished: u64,
    truncated: u64,
    /// Per task: first start hour and rank per available iteration, and
    /// how many of them it finished in.
    starts: Vec<Vec<Option<f64>>>,
    ranks: Vec<Vec<Option<f64>>>,
    finished: Vec<u64>,
    /// Per task: the slot-hours, tokens and USD its started attempts used,
    /// failed, blocked and abandoned ones included, per iteration.
    usage: Vec<[Vec<Option<f64>>; 3]>,
    /// Per complexity: summed slot-hours, tokens and USD per iteration.
    by_complexity: BTreeMap<String, [Vec<Option<f64>>; 3]>,
    rework: [Vec<Option<f64>>; 3],
    arrival_work: [Vec<Option<f64>>; 3],
    followups: Vec<Option<f64>>,
    outside_horizon: u64,
}

impl Outcome {
    fn new(tasks: usize) -> Self {
        Self {
            drain: Vec::new(),
            tokens: Vec::new(),
            usd: Vec::new(),
            critical_path: Vec::new(),
            parallelism: Vec::new(),
            waits: Default::default(),
            return_waits: Vec::new(),
            setbacks: [0; 3],
            no_return: 0,
            attempt_limited: 0,
            available: 0,
            drained: 0,
            complete: 0,
            unfinished: 0,
            truncated: 0,
            starts: vec![Vec::new(); tasks],
            ranks: vec![Vec::new(); tasks],
            finished: vec![0; tasks],
            usage: (0..tasks).map(|_| Default::default()).collect(),
            by_complexity: BTreeMap::new(),
            rework: Default::default(),
            arrival_work: Default::default(),
            followups: Vec::new(),
            outside_horizon: 0,
        }
    }

    fn record(
        &mut self,
        scenario: &Scenario<'_>,
        run: &schedule::Run,
        state: &ScheduleState,
        totals: Draw,
    ) {
        self.drain.push(run.drained);
        for (buffer, draw) in [
            (&mut self.rework, run.rework),
            (&mut self.arrival_work, run.arrivals),
        ] {
            for (values, metric) in buffer.iter_mut().zip([draw.hours, draw.tokens, draw.usd]) {
                values.push(metric);
            }
        }
        self.followups.push(run.waits.map(|_| run.followups as f64));
        self.outside_horizon += run.outside_horizon;
        self.tokens.push(totals.tokens);
        self.usd.push(totals.usd);
        self.critical_path.push(run.critical_path);
        self.parallelism.push(run.parallelism);
        for (slot, values) in self.waits.iter_mut().enumerate() {
            values.push(run.waits.map(|waits| waits[slot]));
        }
        self.return_waits.push(run.return_wait);
        self.truncated += u64::from(run.truncated);
        for (index, included) in scenario.included.iter().take(self.usage.len()).enumerate() {
            if !included {
                continue;
            }
            // Spend is missing wherever the scenario's totals are.
            let used = if run.waits.is_some() {
                state.charged[index]
            } else {
                Draw::MISSING
            };
            for (values, metric) in
                self.usage[index]
                    .iter_mut()
                    .zip([used.hours, used.tokens, used.usd])
            {
                values.push(metric);
            }
        }
        if run.waits.is_none() {
            return;
        }
        self.available += 1;
        for (total, count) in self.setbacks.iter_mut().zip(run.setbacks) {
            *total += count;
        }
        self.no_return += run.no_return as u64;
        self.attempt_limited += run.attempt_limited;
        self.drained += u64::from(run.drained.is_some());
        self.complete += u64::from(run.unfinished == 0);
        self.unfinished += run.unfinished as u64;
        for (index, included) in scenario.included.iter().take(self.starts.len()).enumerate() {
            if !included {
                continue;
            }
            self.starts[index].push(state.start[index]);
            self.ranks[index].push(state.rank[index]);
            self.finished[index] += u64::from(state.finished[index]);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn into_json(
        mut self,
        items: &[Item],
        resolved: &[Resolved],
        scenario: &Scenario<'_>,
        members: &[usize],
        reasons: &[Option<&'static str>],
        rework: &RegressionRework,
        arrivals_supported: bool,
    ) -> Value {
        let included: Vec<usize> = members
            .iter()
            .copied()
            .filter(|index| scenario.included[*index])
            .collect();
        let full = included.len() == members.len();
        let mut counts: BTreeMap<&str, (u64, BTreeMap<&str, u64>)> = BTreeMap::new();
        for index in &included {
            let entry = counts.entry(items[*index].complexity.as_str()).or_default();
            entry.0 += 1;
            *entry.1.entry(resolved[*index].basis.key()).or_default() += 1;
        }
        let mut by_complexity: Vec<(String, [Vec<Option<f64>>; 3])> =
            std::mem::take(&mut self.by_complexity)
                .into_iter()
                .collect();
        by_complexity.sort_by(|(left, _), (right, _)| {
            orbit_types::task::complexity_bucket_ord(left)
                .cmp(&orbit_types::task::complexity_bucket_ord(right))
        });
        let by_complexity: Vec<Value> = by_complexity
            .into_iter()
            .map(|(complexity, [mut hours, mut tokens, mut usd])| {
                let (tasks, basis) = counts.remove(complexity.as_str()).unwrap_or_default();
                json!({
                    "complexity": complexity,
                    "tasks": tasks,
                    "basis": basis,
                    "work_hours": model::forecast_quantiles(&mut hours),
                    "tokens": model::forecast_quantiles(&mut tokens),
                    "usd": model::forecast_quantiles(&mut usd),
                })
            })
            .collect();
        let drain = model::forecast_quantiles(&mut self.drain);
        let tokens = model::forecast_quantiles(&mut self.tokens);
        let usd = model::forecast_quantiles(&mut self.usd);
        let unknown = json!({ "p50": null, "p90": null });
        let mut waits = serde_json::Map::new();
        waits.insert("basis".into(), json!("summed_task_hours"));
        for (wait, values) in ForecastWait::ALL.iter().zip(self.waits.iter_mut()) {
            waits.insert(wait.key().into(), model::forecast_quantiles(values));
        }
        waits.insert(
            "return".into(),
            model::forecast_quantiles(&mut self.return_waits),
        );
        let available = self.available;
        let mean = |count: u64| {
            (available > 0).then(|| (count as f64 / available as f64 * 100.0).round() / 100.0)
        };
        // Probabilities describe the whole scenario, so they are unknown
        // while any task cannot be scheduled.
        let whole = |count: u64| full.then(|| probability(count, available)).flatten();
        let mut predicted: Vec<usize> = members
            .iter()
            .copied()
            .filter(|index| items[*index].in_progress)
            .collect();
        predicted.extend(
            scenario
                .admission
                .dispatch_order()
                .iter()
                .copied()
                .filter(|index| members.contains(index) && !items[*index].in_progress),
        );
        let task_predictions: Vec<Value> = predicted
            .into_iter()
            .map(|index| {
                let item = &items[index];
                let scheduled = scenario.included[index];
                let started = self.starts[index].iter().filter(|at| at.is_some()).count() as u64;
                let [hours, tokens, usd] = &mut self.usage[index];
                json!({
                    "id": item.task_id,
                    "title": item.title,
                    "status": if item.in_progress {
                        "in-progress"
                    } else if item.proposed {
                        "proposed"
                    } else if item.blocked {
                        "blocked"
                    } else {
                        "backlog"
                    },
                    "basis": resolved[index].basis.key(),
                    "outcome_basis": resolved[index].outcome_basis.key(),
                    "return_basis": resolved[index].return_basis.key(),
                    "return_support": {
                        "waits": resolved[index].returns.support,
                        "censored": resolved[index].returns.censored,
                        "tail_unsupported": resolved[index].returns.tail_unsupported,
                    },
                    "review_mode": item.review_mode,
                    "review_mode_source": item.review_mode_source,
                    "review_mode_fallback": resolved[index].review_mode_fallback,
                    "cell_samples": resolved[index].cell_samples,
                    "unforecastable": reasons[index],
                    "profile_source": scenario
                        .admission
                        .profile_source(index)
                        .map(|source| source.as_str()),
                    "predicted_start_hours": model::forecast_quantiles(&mut self.starts[index]),
                    "predicted_start_rank": model::forecast_quantiles(&mut self.ranks[index]),
                    "started_probability": scheduled.then(|| probability(started, available)).flatten(),
                    "completion_probability": scheduled
                        .then(|| probability(self.finished[index], available))
                        .flatten(),
                    // Every attempt it starts by the horizon, as the
                    // scenario totals charge them: stored for scoring.
                    "work_hours": model::forecast_quantiles(hours),
                    "tokens": model::forecast_quantiles(tokens),
                    "usd": model::forecast_quantiles(usd),
                })
            })
            .collect();
        let rework_supported = rework.regression_inputs_supported(&scenario.included);
        let expected = (self.available > 0)
            .then(|| self.followups.iter().flatten().sum::<f64>() / self.available as f64);
        let mut result = json!({
            "tasks": members.len(),
            "time_to_drain_hours": if full { drain.clone() } else { unknown.clone() },
            "tokens": if full { tokens.clone() } else { unknown.clone() },
            "usd": if full { usd.clone() } else { unknown },
            "schedulable_subset": (!full).then(|| json!({
                "tasks": included.len(),
                "of": members.len(),
                "time_to_drain_hours": drain,
                "tokens": tokens,
                "usd": usd,
            })),
            "effective_parallelism": model::forecast_quantiles(&mut self.parallelism),
            "critical_path_hours": model::forecast_quantiles(&mut self.critical_path),
            "wait_hours": waits,
            "completion_probability": whole(self.complete),
            "drain_probability": whole(self.drained),
            "not_expected_to_drain": full.then(|| mean(self.unfinished)).flatten(),
            // Means per simulated drain, over the scheduled tasks.
            // `attempt_limited` is how many tasks started their 25th
            // attempt, not how many pre-generated scripts hold 25 steps.
            "outcomes": {
                "blocked_attempts": mean(self.setbacks[0]),
                "failed_attempts": mean(self.setbacks[1]),
                "abandoned_attempts": mean(self.setbacks[2]),
                "no_return": mean(self.no_return),
                "attempt_limited": mean(self.attempt_limited),
                "max_attempts": MAX_ATTEMPTS,
            },
            "horizon_hours": HORIZON_HOURS,
            "simulation_truncated": self.truncated,
            "by_complexity": by_complexity,
            "task_predictions": task_predictions,
            "regression_rework": {
                "available": rework_supported,
                "generation_limit": 1,
                "expected_followups": if rework_supported { expected } else { None },
                "followups": model::forecast_quantiles(&mut self.followups),
                "work_hours": model::forecast_quantiles(&mut self.rework[0]),
                "tokens": model::forecast_quantiles(&mut self.rework[1]),
                "usd": model::forecast_quantiles(&mut self.rework[2]),
                "detection_outside_horizon": mean(self.outside_horizon),
                "support": rework.regression_history_report(),
            },
            "arrival_work": {
                "work_hours": model::forecast_quantiles(&mut self.arrival_work[0]),
                "tokens": model::forecast_quantiles(&mut self.arrival_work[1]),
                "usd": model::forecast_quantiles(&mut self.arrival_work[2]),
            },
        });
        if !rework_supported || !arrivals_supported {
            let mut baseline = result.clone();
            if let Some(object) = baseline.as_object_mut() {
                object.remove("regression_rework");
                object.remove("task_predictions");
            }
            result["pre_regression_baseline"] = baseline;
            result["unavailable"] = json!({
                "risk_unestimated": members.iter().any(|index| rework.probabilities[*index].is_none()),
                "regression_history_missing": members.iter().any(|index| !rework.history_supported(*index)),
                "regression_history_gaps": rework.member_gaps(members),
                "arrival_conflict_unknown": !arrivals_supported,
            });
            for metric in ["time_to_drain_hours", "tokens", "usd"] {
                result[metric] = json!({"p50": null, "p90": null});
            }
            for metric in [
                "drain_probability",
                "completion_probability",
                "not_expected_to_drain",
            ] {
                result[metric] = Value::Null;
            }
            result["regression_rework"]["detection_outside_horizon"] = Value::Null;
            for metric in ["followups", "work_hours", "tokens", "usd"] {
                result["regression_rework"][metric] = json!({"p50": null, "p90": null});
            }
        }
        result
    }
}

/// The forecast for `items` (backlog, then `if_approved` with proposed).
pub(super) struct Forecast {
    pub(super) backlog: Value,
    pub(super) if_approved: Option<Value>,
}

/// Simulate [`ITERATIONS`] drains of `items` on `slots`, each admitted as
/// the drain would admit it under `admission`, whose task `i` is
/// `items[i]`. Proposed items count only toward `if_approved`; tasks
/// `plan` cannot schedule are left out of each scenario's simulation.
#[allow(clippy::too_many_arguments)]
pub(super) fn simulate_backlog_forecast(
    items: &[Item],
    resolved: &[Resolved],
    history: &[Delivery],
    episodes: &[Episode],
    admission: &ForecastAdmission,
    plan: &DependencyPlan,
    slots: usize,
    rework: &RegressionRework,
    arrivals: Option<&BTreeMap<&'static str, f64>>,
) -> Forecast {
    let frozen = admission;
    let initial_admission =
        frozen.with_forecast_generated_nodes(&(0..items.len()).collect::<Vec<_>>(), items.len());
    let admission = &initial_admission;
    let mut rng = Rng(FORECAST_SEED);
    let has_proposed = items.iter().any(|item| item.proposed);
    let in_progress: Vec<bool> = items.iter().map(|item| item.in_progress).collect();
    let age_hours: Vec<f64> = items.iter().map(|item| item.age_hours).collect();
    let members = |include_proposed: bool| -> Vec<usize> {
        (0..items.len())
            .filter(|index| include_proposed || !items[*index].proposed)
            .collect()
    };
    let scenario = |include_proposed: bool, reasons: &[Option<&'static str>]| {
        let included = (0..items.len())
            .map(|index| (include_proposed || !items[index].proposed) && reasons[index].is_none())
            .collect();
        Scenario::new(
            admission,
            &plan.edges,
            included,
            &in_progress,
            &age_hours,
            slots,
        )
    };
    let backlog_scenario = scenario(false, &plan.backlog);
    let approved_scenario = has_proposed.then(|| scenario(true, &plan.approved));
    let mut outcomes = [Outcome::new(items.len()), Outcome::new(items.len())];
    let mut state = ScheduleState::new(admission);
    let mut hours = vec![None; items.len()];
    let mut scripts: Vec<Script> = (0..items.len()).map(|_| Script::default()).collect();
    for _ in 0..ITERATIONS {
        // Both scenarios replay the same attempts.
        for (index, item) in items.iter().enumerate() {
            let delivery = model::draw_forecast_delivery(&mut rng, &resolved[index], history);
            hours[index] = delivery.hours;
            model::draw_forecast_attempts(
                &mut rng,
                item,
                &resolved[index],
                episodes,
                delivery,
                &mut scripts[index],
            );
        }
        let scenarios = [Some(&backlog_scenario), approved_scenario.as_ref()];
        for (scenario, outcome) in scenarios.into_iter().zip(outcomes.iter_mut()) {
            let Some(scenario) = scenario else {
                continue;
            };
            let supported = rework.regression_inputs_supported(&scenario.included);
            let (mut generated, mut truncated) = if supported {
                rework::draw_regression_bundles(
                    rework,
                    items,
                    &scenario.included,
                    episodes,
                    &mut rng,
                )
            } else {
                (Vec::new(), false)
            };
            if let Some(rates) = arrivals {
                truncated |= rework::draw_forecast_arrivals(
                    rework,
                    items.len(),
                    rates,
                    episodes,
                    &mut rng,
                    &mut generated,
                );
            }
            let templates: Vec<usize> = (0..items.len())
                .chain(generated.iter().map(|node| node.template))
                .collect();
            let expanded = (!generated.is_empty())
                .then(|| frozen.with_forecast_generated_nodes(&templates, items.len()));
            let mut edges = plan.edges.clone();
            edges.extend(
                generated
                    .iter()
                    .map(|node| node.origin.into_iter().collect::<Vec<_>>()),
            );
            let mut included = scenario.included.clone();
            included.extend(
                generated
                    .iter()
                    .map(|node| node.origin.is_none_or(|origin| scenario.included[origin])),
            );
            let mut running = in_progress.clone();
            running.resize(templates.len(), false);
            let mut ages = age_hours.clone();
            ages.resize(templates.len(), 0.0);
            let mut expanded_scenario = Scenario::new(
                expanded.as_ref().unwrap_or(admission),
                &edges,
                included,
                &running,
                &ages,
                slots,
            );
            expanded_scenario.initial = items.len();
            expanded_scenario.generated = &generated;
            let mut expanded_state = expanded.as_ref().map(ScheduleState::new);
            let state = expanded_state.as_mut().unwrap_or(&mut state);
            let mut scripts_all = scripts.clone();
            scripts_all.extend(generated.iter().map(|node| node.script.clone()));
            let mut hours_all = hours.clone();
            hours_all.extend(generated.iter().map(|node| Some(node.hours)));
            let mut totals = Draw::ZERO;
            let mut run = schedule::simulate_drain(
                &expanded_scenario,
                &hours_all,
                &scripts_all,
                &mut totals,
                state,
            );
            if truncated {
                run.truncated = true;
                run.waits = None;
                run.drained = None;
                run.rework = Draw::MISSING;
                run.arrivals = Draw::MISSING;
            }
            // Spend is what the attempts started by the horizon used.
            let mut complexity: BTreeMap<&str, Draw> = BTreeMap::new();
            for (index, item) in items.iter().enumerate() {
                if !scenario.included[index] {
                    continue;
                }
                let cell = complexity
                    .entry(item.complexity.as_str())
                    .or_insert(Draw::ZERO);
                if run.waits.is_some() {
                    cell.add(&state.charged[index]);
                } else {
                    *cell = Draw::MISSING;
                }
            }
            if run.waits.is_none() {
                totals = Draw::MISSING;
            }
            outcome.record(scenario, &run, state, totals);
            for (key, cell) in complexity {
                let entry = outcome.by_complexity.entry(key.to_string()).or_default();
                entry[0].push(cell.hours);
                entry[1].push(cell.tokens);
                entry[2].push(cell.usd);
            }
        }
    }
    let [backlog_outcome, approved_outcome] = outcomes;
    let backlog = backlog_outcome.into_json(
        items,
        resolved,
        &backlog_scenario,
        &members(false),
        &plan.backlog,
        rework,
        arrivals
            .is_none_or(|rates| rates.values().sum::<f64>() <= 0.0 || !rework.donors.is_empty()),
    );
    let if_approved = approved_scenario.as_ref().map(|scenario| {
        approved_outcome.into_json(
            items,
            resolved,
            scenario,
            &members(true),
            &plan.approved,
            rework,
            arrivals.is_none_or(|rates| {
                rates.values().sum::<f64>() <= 0.0 || !rework.donors.is_empty()
            }),
        )
    });
    Forecast {
        backlog,
        if_approved,
    }
}
