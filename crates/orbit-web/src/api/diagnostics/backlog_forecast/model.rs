//! The forecast model: per-task history cells and the Monte Carlo drain.
//!
//! Pure computation over values the endpoint has already read; no store or
//! runtime access, so the same inputs always give the same quantiles (the
//! generator is seeded with a constant).

use std::collections::{BTreeMap, BTreeSet};

use orbit_types::task::UNSET_BUCKET;
use serde_json::{Value, json};

/// Bumped whenever the sampling or simulation changes what a stored
/// forecast means, so calibration can tell versions apart.
pub(super) const MODEL_VERSION: u32 = 1;
/// Simulated drains per forecast.
pub(super) const ITERATIONS: usize = 1_000;
/// A complexity × crew-pool cell with fewer completed deliveries than this
/// falls back to the complexity's whole history.
pub(super) const MIN_CELL_SAMPLES: usize = 5;
/// With arrivals on, a simulated drain that has not emptied the queue by
/// this many hours stops and counts as beyond the horizon.
pub(super) const ARRIVALS_HORIZON_HOURS: f64 = 90.0 * 24.0;
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

pub(super) fn resolve(item: &Item, history: &[Delivery]) -> Resolved {
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
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn index(&mut self, len: usize) -> usize {
        (self.next() % len as u64) as usize
    }

    /// Uniform in (0, 1].
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }
}

/// One task's draw for one simulated drain, or a sum of draws. A missing
/// value (no history, or no priced or costed delivery to draw from) makes
/// every sum it joins missing, so a total never silently undercounts.
#[derive(Clone, Copy)]
struct Draw {
    hours: Option<f64>,
    tokens: Option<f64>,
    usd: Option<f64>,
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

    fn add(&mut self, other: &Draw) {
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

/// List scheduling: each task, in order, starts on the slot that frees
/// first, no earlier than it is available.
struct Slots {
    free: Vec<f64>,
    end: f64,
}

impl Slots {
    fn new(slots: usize) -> Self {
        Self {
            free: vec![0.0; slots.max(1)],
            end: 0.0,
        }
    }

    /// Place one task; returns when the last placed task ends.
    fn place(&mut self, available: f64, hours: f64) -> f64 {
        let mut slot = 0;
        for (index, at) in self.free.iter().enumerate() {
            if *at < self.free[slot] {
                slot = index;
            }
        }
        let finish = self.free[slot].max(available) + hours;
        self.free[slot] = finish;
        self.end = self.end.max(finish);
        self.end
    }
}

/// The opt-in arrivals: `per_day` new tasks a day, each drawn from the
/// whole history.
pub(super) struct Arrivals<'a> {
    pub(super) per_day: f64,
    pub(super) history: &'a [Delivery],
}

/// One simulated drain of `backlog` on `slots`. With arrivals, new tasks
/// keep landing until the queue empties; `None` when it has not emptied by
/// [`ARRIVALS_HORIZON_HOURS`], or when a task has no history to draw its
/// hours from. Arrived work adds its known tokens and cost to `totals`.
fn drain(
    rng: &mut Rng,
    slots: usize,
    backlog: &[Draw],
    arrivals: Option<&Arrivals<'_>>,
    totals: &mut Draw,
) -> Option<f64> {
    let mut schedule = Slots::new(slots);
    let mut current = 0.0;
    for job in backlog {
        current = schedule.place(0.0, job.hours?);
    }
    let Some(arrivals) = arrivals.filter(|a| a.per_day > 0.0 && !a.history.is_empty()) else {
        return Some(current);
    };
    let rate_per_hour = arrivals.per_day / 24.0;
    let mut at = 0.0;
    loop {
        at += -rng.unit().ln() / rate_per_hour;
        // The queue emptied before this arrival: the drain is done.
        if at >= current {
            return Some(current);
        }
        let delivery = &arrivals.history[rng.index(arrivals.history.len())];
        totals.add(&Draw {
            hours: Some(delivery.hours),
            tokens: Some(delivery.tokens.unwrap_or(0.0)),
            usd: Some(delivery.usd.unwrap_or(0.0)),
        });
        current = schedule.place(at, delivery.hours);
        if current > ARRIVALS_HORIZON_HOURS {
            return None;
        }
    }
}

/// p50 and p90 of `values` by nearest rank; `None` entries (beyond the
/// horizon, or not computable) sort last.
fn quantiles(values: &mut [Option<f64>]) -> Value {
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

/// The simulated totals for one set of tasks.
#[derive(Default)]
struct Outcome {
    drain: Vec<Option<f64>>,
    tokens: Vec<Option<f64>>,
    usd: Vec<Option<f64>>,
    /// Per complexity: summed slot-hours, tokens and USD per iteration.
    by_complexity: BTreeMap<String, [Vec<Option<f64>>; 3]>,
}

impl Outcome {
    fn into_json(mut self, items: &[&Item], resolved: &[&Resolved]) -> Value {
        let mut counts: BTreeMap<&str, (u64, BTreeMap<&str, u64>)> = BTreeMap::new();
        for (item, resolved) in items.iter().zip(resolved) {
            let entry = counts.entry(item.complexity.as_str()).or_default();
            entry.0 += 1;
            *entry.1.entry(resolved.basis.key()).or_default() += 1;
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
        json!({
            "tasks": items.len(),
            "time_to_drain_hours": quantiles(&mut self.drain),
            "tokens": quantiles(&mut self.tokens),
            "usd": quantiles(&mut self.usd),
            "by_complexity": by_complexity,
        })
    }
}

/// The forecast for `items` (backlog, then `if_approved` with proposed).
pub(super) struct Forecast {
    pub(super) backlog: Value,
    pub(super) if_approved: Option<Value>,
}

/// Simulate [`ITERATIONS`] drains of `items` on `slots`. Items are in
/// dispatch order with in-progress ones first; proposed ones count only
/// toward `if_approved`.
pub(super) fn simulate(
    items: &[Item],
    resolved: &[Resolved],
    history: &[Delivery],
    slots: usize,
    arrivals: Option<&Arrivals<'_>>,
) -> Forecast {
    let mut rng = Rng(SEED);
    let has_proposed = items.iter().any(|item| item.proposed);
    let mut backlog = Outcome::default();
    let mut approved = Outcome::default();
    let mut draws = vec![Draw::MISSING; items.len()];
    for _ in 0..ITERATIONS {
        for (index, item) in items.iter().enumerate() {
            draws[index] = draw(&mut rng, &resolved[index], history, item.spent);
        }
        for (outcome, include_proposed) in [(&mut backlog, false), (&mut approved, true)] {
            if include_proposed && !has_proposed {
                continue;
            }
            let chosen: Vec<(usize, &Draw)> = draws
                .iter()
                .enumerate()
                .filter(|(index, _)| include_proposed || !items[*index].proposed)
                .collect();
            let mut totals = Draw::ZERO;
            let mut complexity: BTreeMap<&str, Draw> = BTreeMap::new();
            for (index, one) in &chosen {
                totals.add(one);
                complexity
                    .entry(items[*index].complexity.as_str())
                    .or_insert(Draw::ZERO)
                    .add(one);
            }
            let jobs: Vec<Draw> = chosen.iter().map(|(_, one)| **one).collect();
            let drained = drain(&mut rng, slots, &jobs, arrivals, &mut totals);
            outcome.drain.push(drained);
            outcome.tokens.push(totals.tokens);
            outcome.usd.push(totals.usd);
            for (key, cell) in complexity {
                let entry = outcome.by_complexity.entry(key.to_string()).or_default();
                entry[0].push(cell.hours);
                entry[1].push(cell.tokens);
                entry[2].push(cell.usd);
            }
        }
    }
    let pick = |include_proposed: bool| -> (Vec<&Item>, Vec<&Resolved>) {
        items
            .iter()
            .zip(resolved)
            .filter(|(item, _)| include_proposed || !item.proposed)
            .unzip()
    };
    let (backlog_items, backlog_resolved) = pick(false);
    let backlog = backlog.into_json(&backlog_items, &backlog_resolved);
    let if_approved = has_proposed.then(|| {
        let (all_items, all_resolved) = pick(true);
        approved.into_json(&all_items, &all_resolved)
    });
    Forecast {
        backlog,
        if_approved,
    }
}
