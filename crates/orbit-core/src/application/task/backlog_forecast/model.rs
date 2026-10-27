//! The forecast model: per-task history cells and the Monte Carlo drain,
//! each iteration scheduled as the drain admits work (`schedule.rs`).
//!
//! Pure computation over values the forecast service has already read; no store or
//! runtime access, so the same inputs always give the same quantiles (the
//! generator is seeded with a constant).

use std::collections::{BTreeMap, BTreeSet};

use orbit_types::task::UNSET_BUCKET;
use serde_json::{Value, json};

use super::dependencies::DependencyPlan;
use super::schedule::{self, HORIZON_HOURS, Scenario, ScheduleState};
use crate::application::admission::forecast::{ForecastAdmission, ForecastWait};

/// Bumped whenever the sampling or simulation changes what a stored
/// forecast means, so calibration can tell versions apart. 3: the drain's
/// dependency, lock, unknown-footprint, reservation and dispatch-order
/// admission [ORB-18686].
pub(super) const MODEL_VERSION: u32 = 3;
/// The revision of each backlog-forecast-v2 factor this model applies;
/// `None` for one not yet modelled.
pub(super) const ADMISSION_FACTORS: [(&str, Option<u32>); 5] = [
    ("admission", Some(1)),
    ("review_modes", None),
    ("outcomes", None),
    ("resource_fit", None),
    ("regression_rework", None),
];
/// Simulated drains per forecast.
pub(super) const ITERATIONS: usize = 1_000;
/// A complexity × crew-pool cell with fewer completed deliveries than this
/// falls back to the complexity's whole history.
pub(super) const MIN_CELL_SAMPLES: usize = 5;
const SEED: u64 = 0x0b17_f0ec_a57d_2026;

/// One completed delivery: a done task with linked invocations, as the
/// history sample a backlog task draws.
#[derive(Clone)]
pub(super) struct Delivery {
    pub(super) complexity: String,
    pub(super) crew: Option<String>,
    pub(super) hours: f64,
    /// `None` when an invocation ran a model the token normalization does
    /// not cover.
    pub(super) tokens: Option<f64>,
    /// `None` when an invocation recorded no valid provider cost.
    pub(super) usd: Option<f64>,
}

/// What an in-progress task has already used, subtracted from its draw.
#[derive(Clone, Copy, Default)]
pub(super) struct Spent {
    pub(super) hours: f64,
    pub(super) tokens: f64,
    pub(super) usd: f64,
}

/// One task the forecast covers.
pub(super) struct Item {
    pub(super) task_id: String,
    pub(super) title: String,
    /// Complexity bucket; [`UNSET_BUCKET`] when unassessed.
    pub(super) complexity: String,
    /// The crews it will draw: its own crew, else its complexity pool.
    pub(super) crews: BTreeSet<String>,
    pub(super) in_progress: bool,
    pub(super) proposed: bool,
    pub(super) spent: Spent,
}

/// Which history a task's draws come from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Basis {
    /// Its complexity × crew-pool cell.
    Cell,
    /// The cell was thin: every delivery of its complexity.
    Complexity,
    /// Unassessed, or its complexity has no history: every delivery.
    Workspace,
    /// The lookback holds no delivery at all.
    None,
}

impl Basis {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Cell => "cell",
            Self::Complexity => "complexity",
            Self::Workspace => "workspace",
            Self::None => "none",
        }
    }
}

/// A task's resolved history: indices into the delivery list.
pub(super) struct Resolved {
    pub(super) basis: Basis,
    /// Size of its own cell, before any fallback.
    pub(super) cell_samples: usize,
    samples: Vec<usize>,
    token_samples: Vec<usize>,
    cost_samples: Vec<usize>,
}

impl Resolved {
    /// No delivery in its history ran only covered models.
    pub(super) fn uncovered_model(&self) -> bool {
        !self.samples.is_empty() && self.token_samples.is_empty()
    }

    /// No delivery in its history recorded a provider cost throughout.
    pub(super) fn uncosted(&self) -> bool {
        !self.samples.is_empty() && self.cost_samples.is_empty()
    }
}

pub(super) fn resolve_forecast_item(item: &Item, history: &[Delivery]) -> Resolved {
    let assessed = item.complexity != UNSET_BUCKET;
    let of_complexity: Vec<usize> = (0..history.len())
        .filter(|index| assessed && history[*index].complexity == item.complexity)
        .collect();
    let cell: Vec<usize> = of_complexity
        .iter()
        .copied()
        .filter(|index| {
            history[*index]
                .crew
                .as_ref()
                .is_some_and(|crew| item.crews.contains(crew))
        })
        .collect();
    let cell_samples = cell.len();
    let (basis, samples) = if cell.len() >= MIN_CELL_SAMPLES {
        (Basis::Cell, cell)
    } else if !of_complexity.is_empty() {
        (Basis::Complexity, of_complexity)
    } else if !history.is_empty() {
        (Basis::Workspace, (0..history.len()).collect())
    } else {
        (Basis::None, Vec::new())
    };
    let token_samples = samples
        .iter()
        .copied()
        .filter(|index| history[*index].tokens.is_some())
        .collect();
    let cost_samples = samples
        .iter()
        .copied()
        .filter(|index| history[*index].usd.is_some())
        .collect();
    Resolved {
        basis,
        cell_samples,
        samples,
        token_samples,
        cost_samples,
    }
}

/// splitmix64: small, seedable and good enough for resampling.
pub(super) struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub(super) fn index(&mut self, len: usize) -> usize {
        (self.next() % len as u64) as usize
    }

    /// Uniform in (0, 1].
    pub(super) fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }
}

/// One task's draw for one simulated drain, or a sum of draws. A missing
/// value (no history, or no priced or costed delivery to draw from) makes
/// every sum it joins missing, so a total never silently undercounts.
#[derive(Clone, Copy)]
pub(super) struct Draw {
    pub(super) hours: Option<f64>,
    pub(super) tokens: Option<f64>,
    pub(super) usd: Option<f64>,
}

impl Draw {
    const ZERO: Self = Self {
        hours: Some(0.0),
        tokens: Some(0.0),
        usd: Some(0.0),
    };
    const MISSING: Self = Self {
        hours: None,
        tokens: None,
        usd: None,
    };

    pub(super) fn add(&mut self, other: &Draw) {
        let sum = |total: Option<f64>, one: Option<f64>| total.zip(one).map(|(a, b)| a + b);
        self.hours = sum(self.hours, other.hours);
        self.tokens = sum(self.tokens, other.tokens);
        self.usd = sum(self.usd, other.usd);
    }
}

fn draw(rng: &mut Rng, resolved: &Resolved, history: &[Delivery], spent: Spent) -> Draw {
    if resolved.samples.is_empty() {
        return Draw::MISSING;
    }
    // The delivery is drawn whole, so a slow delivery's tokens and cost come
    // with it; a missing token or cost value is redrawn from the deliveries
    // of the same history that recorded one.
    let delivery = &history[resolved.samples[rng.index(resolved.samples.len())]];
    let tokens = delivery.tokens.or_else(|| {
        (!resolved.token_samples.is_empty()).then_some(())?;
        history[resolved.token_samples[rng.index(resolved.token_samples.len())]].tokens
    });
    let usd = delivery.usd.or_else(|| {
        (!resolved.cost_samples.is_empty()).then_some(())?;
        history[resolved.cost_samples[rng.index(resolved.cost_samples.len())]].usd
    });
    Draw {
        hours: Some((delivery.hours - spent.hours).max(0.0)),
        tokens: tokens.map(|tokens| (tokens - spent.tokens).max(0.0)),
        usd: usd.map(|usd| (usd - spent.usd).max(0.0)),
    }
}

/// The opt-in arrivals: `per_day` new tasks a day, each drawn from the
/// whole history.
pub(super) struct Arrivals<'a> {
    pub(super) per_day: f64,
    pub(super) history: &'a [Delivery],
}

/// p50 and p90 of `values` by nearest rank; `None` entries (beyond the
/// horizon, or not computable) sort last.
pub(super) fn quantiles(values: &mut [Option<f64>]) -> Value {
    if values.is_empty() {
        return json!({ "p50": null, "p90": null });
    }
    values.sort_by(|left, right| match (left, right) {
        (Some(left), Some(right)) => left.total_cmp(right),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let rank = |q: f64| {
        let index = ((q * values.len() as f64).ceil() as usize).clamp(1, values.len()) - 1;
        values[index].map(round)
    };
    json!({ "p50": rank(0.5), "p90": rank(0.9) })
}

fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Quantiles of a plain sample set (a task's own history).
pub(super) fn sample_quantiles(mut values: Vec<f64>, less: f64) -> Value {
    let mut values: Vec<Option<f64>> = values
        .drain(..)
        .map(|value| Some((value - less).max(0.0)))
        .collect();
    quantiles(&mut values)
}

impl Resolved {
    /// This task's own p50/p90 of hours, tokens and USD, as stored for
    /// per-task scoring.
    pub(super) fn marginals(&self, history: &[Delivery], spent: Spent) -> Value {
        let pick = |indices: &[usize], value: &dyn Fn(&Delivery) -> Option<f64>| {
            indices
                .iter()
                .filter_map(|index| value(&history[*index]))
                .collect::<Vec<_>>()
        };
        json!({
            "hours": sample_quantiles(pick(&self.samples, &|d| Some(d.hours)), spent.hours),
            "tokens": sample_quantiles(pick(&self.token_samples, &|d| d.tokens), spent.tokens),
            "usd": sample_quantiles(pick(&self.cost_samples, &|d| d.usd), spent.usd),
        })
    }
}

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
    waits: [Vec<Option<f64>>; 5],
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
    /// Per complexity: summed slot-hours, tokens and USD per iteration.
    by_complexity: BTreeMap<String, [Vec<Option<f64>>; 3]>,
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
            available: 0,
            drained: 0,
            complete: 0,
            unfinished: 0,
            truncated: 0,
            starts: vec![Vec::new(); tasks],
            ranks: vec![Vec::new(); tasks],
            finished: vec![0; tasks],
            by_complexity: BTreeMap::new(),
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
        self.tokens.push(totals.tokens);
        self.usd.push(totals.usd);
        self.critical_path.push(run.critical_path);
        self.parallelism.push(run.parallelism);
        for (slot, values) in self.waits.iter_mut().enumerate() {
            values.push(run.waits.map(|waits| waits[slot]));
        }
        self.truncated += u64::from(run.truncated);
        if run.waits.is_none() {
            return;
        }
        self.available += 1;
        self.drained += u64::from(run.drained.is_some());
        self.complete += u64::from(run.unfinished == 0);
        self.unfinished += run.unfinished as u64;
        for (index, included) in scenario.included.iter().enumerate() {
            if !included {
                continue;
            }
            self.starts[index].push(state.start[index]);
            self.ranks[index].push(state.rank[index]);
            self.finished[index] += u64::from(state.finished[index]);
        }
    }

    fn into_json(
        mut self,
        items: &[Item],
        resolved: &[Resolved],
        scenario: &Scenario<'_>,
        members: &[usize],
        reasons: &[Option<&'static str>],
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
                    "work_hours": quantiles(&mut hours),
                    "tokens": quantiles(&mut tokens),
                    "usd": quantiles(&mut usd),
                })
            })
            .collect();
        let drain = quantiles(&mut self.drain);
        let tokens = quantiles(&mut self.tokens);
        let usd = quantiles(&mut self.usd);
        let unknown = json!({ "p50": null, "p90": null });
        let mut waits = serde_json::Map::new();
        waits.insert("basis".into(), json!("summed_task_hours"));
        for (wait, values) in ForecastWait::ALL.iter().zip(self.waits.iter_mut()) {
            waits.insert(wait.key().into(), quantiles(values));
        }
        let available = self.available;
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
                json!({
                    "id": item.task_id,
                    "title": item.title,
                    "status": if item.in_progress {
                        "in-progress"
                    } else if item.proposed {
                        "proposed"
                    } else {
                        "backlog"
                    },
                    "basis": resolved[index].basis.key(),
                    "unforecastable": reasons[index],
                    "predicted_start_hours": quantiles(&mut self.starts[index]),
                    "predicted_start_rank": quantiles(&mut self.ranks[index]),
                    "started_probability": scheduled.then(|| probability(started, available)).flatten(),
                    "completion_probability": scheduled
                        .then(|| probability(self.finished[index], available))
                        .flatten(),
                })
            })
            .collect();
        json!({
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
            "effective_parallelism": quantiles(&mut self.parallelism),
            "critical_path_hours": quantiles(&mut self.critical_path),
            "wait_hours": waits,
            "completion_probability": whole(self.complete),
            "drain_probability": whole(self.drained),
            "not_expected_to_drain": full
                .then(|| (available > 0).then(|| {
                    (self.unfinished as f64 / available as f64 * 100.0).round() / 100.0
                }))
                .flatten(),
            "horizon_hours": HORIZON_HOURS,
            "simulation_truncated": self.truncated,
            "by_complexity": by_complexity,
            "task_predictions": task_predictions,
        })
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
pub(super) fn simulate_backlog_forecast(
    items: &[Item],
    resolved: &[Resolved],
    history: &[Delivery],
    admission: &ForecastAdmission,
    plan: &DependencyPlan,
    slots: usize,
    arrivals: Option<&Arrivals<'_>>,
) -> Forecast {
    let mut rng = Rng(SEED);
    let has_proposed = items.iter().any(|item| item.proposed);
    let in_progress: Vec<bool> = items.iter().map(|item| item.in_progress).collect();
    let members = |include_proposed: bool| -> Vec<usize> {
        (0..items.len())
            .filter(|index| include_proposed || !items[*index].proposed)
            .collect()
    };
    let scenario = |include_proposed: bool, reasons: &[Option<&'static str>]| {
        let included = (0..items.len())
            .map(|index| (include_proposed || !items[index].proposed) && reasons[index].is_none())
            .collect();
        Scenario::new(admission, &plan.edges, included, &in_progress, slots)
    };
    let backlog_scenario = scenario(false, &plan.backlog);
    let approved_scenario = has_proposed.then(|| scenario(true, &plan.approved));
    let mut outcomes = [Outcome::new(items.len()), Outcome::new(items.len())];
    let mut state = ScheduleState::new(admission);
    let mut draws = vec![Draw::MISSING; items.len()];
    let mut hours = vec![None; items.len()];
    for _ in 0..ITERATIONS {
        for (index, item) in items.iter().enumerate() {
            draws[index] = draw(&mut rng, &resolved[index], history, item.spent);
            hours[index] = draws[index].hours;
        }
        let scenarios = [Some(&backlog_scenario), approved_scenario.as_ref()];
        for (scenario, outcome) in scenarios.into_iter().zip(outcomes.iter_mut()) {
            let Some(scenario) = scenario else {
                continue;
            };
            let mut totals = Draw::ZERO;
            let mut complexity: BTreeMap<&str, Draw> = BTreeMap::new();
            for (index, one) in draws.iter().enumerate() {
                if !scenario.included[index] {
                    continue;
                }
                totals.add(one);
                complexity
                    .entry(items[index].complexity.as_str())
                    .or_insert(Draw::ZERO)
                    .add(one);
            }
            let run = schedule::simulate_drain(
                scenario,
                &hours,
                arrivals,
                &mut rng,
                &mut totals,
                &mut state,
            );
            outcome.record(scenario, &run, &state, totals);
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
    );
    let if_approved = approved_scenario.as_ref().map(|scenario| {
        approved_outcome.into_json(items, resolved, scenario, &members(true), &plan.approved)
    });
    Forecast {
        backlog,
        if_approved,
    }
}
