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
use super::outcomes::{AwayWait, Episode, EpisodeOutcome, Follow, MAX_ATTEMPTS, ReturnCurve};
use super::review_modes::ReviewMode;
use super::schedule::{self, HORIZON_HOURS, Scenario, ScheduleState};
use crate::application::admission::forecast::{ForecastAdmission, ForecastWait};

/// Bumped whenever the sampling or simulation changes what a stored
/// forecast means, so calibration can tell versions apart. 4: review-mode
/// sampling and explicit mode-pooled fallback [ORB-18687]. 5: blocked,
/// failed and abandoned attempts, returns and blocked tasks [ORB-18688].
/// 6: gap fit holds work below the ceiling where the drain enforces it
/// [ORB-18689]. 7: sub-second and same-instant returns stay drawable for
/// a fresh blocked attempt [ORB-18853]. 8: a censored blocked wait is
/// summed through the horizon, and absorption stops at its own event
/// [ORB-18856].
pub(super) const MODEL_VERSION: u32 = 8;
/// The revision of each backlog-forecast-v2 factor this model applies;
/// `None` for one not yet modelled.
pub(super) const ADMISSION_FACTORS: [(&str, Option<u32>); 5] = [
    ("admission", Some(1)),
    ("review_modes", Some(1)),
    ("outcomes", Some(1)),
    ("resource_fit", Some(1)),
    ("regression_rework", None),
];
/// Simulated drains per forecast.
pub(super) const ITERATIONS: usize = 1_000;
/// Each mode-preserving pool requires this many completed deliveries.
pub(super) const MIN_CELL_SAMPLES: usize = 5;
const SEED: u64 = 0x0b17_f0ec_a57d_2026;

/// One completed delivery: a done task with linked invocations since its
/// last attempt that did not deliver, as the history sample a task's
/// delivering attempt draws.
#[derive(Clone)]
pub(super) struct Delivery {
    pub(super) complexity: String,
    pub(super) crew: Option<String>,
    pub(super) review_mode: ReviewMode,
    pub(super) hours: f64,
    /// `None` when an invocation ran a model the token normalization does
    /// not cover.
    pub(super) tokens: Option<f64>,
    /// `None` when an invocation recorded no valid provider cost.
    pub(super) usd: Option<f64>,
}

/// What an in-progress task's current attempt has already used, subtracted
/// from its draw.
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
    pub(super) review_mode: ReviewMode,
    pub(super) review_mode_source: String,
    pub(super) in_progress: bool,
    pub(super) proposed: bool,
    /// Blocked now: it starts waiting for a return.
    pub(super) blocked: bool,
    /// Hours since the running attempt was admitted, or since the task was
    /// blocked; zero when the history does not say.
    pub(super) age_hours: f64,
    pub(super) spent: Spent,
}

/// A history row a task's cell can draw.
pub(super) trait CellRow {
    fn complexity(&self) -> &str;
    fn crew(&self) -> Option<&str>;
    fn review_mode(&self) -> ReviewMode;
}

impl CellRow for Delivery {
    fn complexity(&self) -> &str {
        &self.complexity
    }
    fn crew(&self) -> Option<&str> {
        self.crew.as_deref()
    }
    fn review_mode(&self) -> ReviewMode {
        self.review_mode
    }
}

/// Which history a task's draws come from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Basis {
    /// Its complexity × crew-pool × review-mode cell.
    Cell,
    /// Drop crew while preserving complexity and review mode.
    ReviewComplexity,
    /// Drop complexity too, preserving review mode.
    ReviewWorkspace,
    /// Review support was thin: the mode-pooled complexity × crew cell.
    ModePooledCell,
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
            Self::ReviewComplexity => "review_complexity",
            Self::ReviewWorkspace => "review_workspace",
            Self::ModePooledCell => "mode_pooled_cell",
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
    /// The resolved pool does not preserve the assumed review mode.
    pub(super) review_mode_fallback: bool,
    samples: Vec<usize>,
    token_samples: Vec<usize>,
    cost_samples: Vec<usize>,
    /// Where its attempt outcomes come from: indices into the episodes.
    pub(super) outcome_basis: Basis,
    episodes: Vec<usize>,
    setback_tokens: Vec<usize>,
    setback_costs: Vec<usize>,
    /// Its returns after leaving the queue.
    pub(super) return_basis: Basis,
    pub(super) returns: ReturnCurve,
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

/// A resolved pool: its basis and indices into its rows.
struct Pool {
    basis: Basis,
    cell_samples: usize,
    review_mode_fallback: bool,
    samples: Vec<usize>,
}

/// The cell fallback chain over any history rows: complexity × crew pool ×
/// review mode, dropping crew then complexity while preserving mode at
/// [`MIN_CELL_SAMPLES`], then the mode-pooled cell, complexity and
/// workspace.
fn resolve_pool<R: CellRow>(item: &Item, rows: &[R]) -> Pool {
    let assessed = item.complexity != UNSET_BUCKET;
    let of_complexity: Vec<usize> = (0..rows.len())
        .filter(|index| assessed && rows[*index].complexity() == item.complexity)
        .collect();
    let pooled_cell: Vec<usize> = of_complexity
        .iter()
        .copied()
        .filter(|index| {
            rows[*index]
                .crew()
                .is_some_and(|crew| item.crews.contains(crew))
        })
        .collect();
    // Unknown is missing evidence, never a measured policy choice.
    let matches_mode = |index: &usize| {
        item.review_mode != ReviewMode::Unknown && rows[*index].review_mode() == item.review_mode
    };
    let cell: Vec<usize> = pooled_cell.iter().copied().filter(matches_mode).collect();
    let mode_complexity: Vec<usize> = of_complexity.iter().copied().filter(matches_mode).collect();
    let mode_workspace: Vec<usize> = (0..rows.len()).filter(matches_mode).collect();
    let cell_samples = cell.len();
    let (basis, samples) = if cell.len() >= MIN_CELL_SAMPLES {
        (Basis::Cell, cell)
    } else if mode_complexity.len() >= MIN_CELL_SAMPLES {
        (Basis::ReviewComplexity, mode_complexity)
    } else if mode_workspace.len() >= MIN_CELL_SAMPLES {
        (Basis::ReviewWorkspace, mode_workspace)
    } else if pooled_cell.len() >= MIN_CELL_SAMPLES {
        (Basis::ModePooledCell, pooled_cell)
    } else if !of_complexity.is_empty() {
        (Basis::Complexity, of_complexity)
    } else if !rows.is_empty() {
        (Basis::Workspace, (0..rows.len()).collect())
    } else {
        (Basis::None, Vec::new())
    };
    Pool {
        basis,
        cell_samples,
        review_mode_fallback: !matches!(
            basis,
            Basis::Cell | Basis::ReviewComplexity | Basis::ReviewWorkspace
        ),
        samples,
    }
}

pub(super) fn resolve_forecast_item(
    item: &Item,
    history: &[Delivery],
    episodes: &[Episode],
    waits: &[AwayWait],
) -> Resolved {
    let pool = resolve_pool(item, history);
    let token_samples = pool
        .samples
        .iter()
        .copied()
        .filter(|index| history[*index].tokens.is_some())
        .collect();
    let cost_samples = pool
        .samples
        .iter()
        .copied()
        .filter(|index| history[*index].usd.is_some())
        .collect();
    let outcomes = resolve_pool(item, episodes);
    let setbacks = |metric: &dyn Fn(&Episode) -> Option<f64>| -> Vec<usize> {
        outcomes
            .samples
            .iter()
            .copied()
            .filter(|index| {
                episodes[*index].outcome != EpisodeOutcome::Delivered
                    && metric(&episodes[*index]).is_some()
            })
            .collect()
    };
    let setback_tokens = setbacks(&|episode| episode.tokens);
    let setback_costs = setbacks(&|episode| episode.usd);
    let returns = resolve_pool(item, waits);
    Resolved {
        basis: pool.basis,
        cell_samples: pool.cell_samples,
        review_mode_fallback: pool.review_mode_fallback,
        samples: pool.samples,
        token_samples,
        cost_samples,
        outcome_basis: outcomes.basis,
        episodes: outcomes.samples,
        setback_tokens,
        setback_costs,
        return_basis: returns.basis,
        returns: ReturnCurve::estimate(waits, &returns.samples),
    }
}

impl Resolved {
    /// The share of its outcome pool's attempts that did not deliver;
    /// `None` without episodes.
    pub(super) fn setback_probability(&self, episodes: &[Episode]) -> Option<f64> {
        (!self.episodes.is_empty()).then(|| {
            let setbacks = self
                .episodes
                .iter()
                .filter(|index| episodes[**index].outcome != EpisodeOutcome::Delivered)
                .count();
            round_share(setbacks as f64 / self.episodes.len() as f64)
        })
    }

    /// Whether its outcome pool holds an attempt that left the queue.
    pub(super) fn can_block(&self, episodes: &[Episode]) -> bool {
        self.episodes
            .iter()
            .any(|index| episodes[*index].outcome == EpisodeOutcome::Blocked)
    }
}

fn round_share(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
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
    pub(super) const ZERO: Self = Self {
        hours: Some(0.0),
        tokens: Some(0.0),
        usd: Some(0.0),
    };
    pub(super) const MISSING: Self = Self {
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

    /// This draw less what was already used, floored at zero.
    fn less(self, spent: Spent) -> Self {
        let less = |value: Option<f64>, used: f64| value.map(|value| (value - used).max(0.0));
        Self {
            hours: less(self.hours, spent.hours),
            tokens: less(self.tokens, spent.tokens),
            usd: less(self.usd, spent.usd),
        }
    }
}

/// One attempt in a task's simulated drain.
#[derive(Clone, Copy)]
pub(super) struct Step {
    /// The slot hours it holds.
    pub(super) hours: f64,
    /// What it uses, charged when it starts.
    pub(super) spend: Draw,
    pub(super) outcome: EpisodeOutcome,
    /// What follows an attempt that did not deliver.
    pub(super) then: Follow,
}

/// A task's attempts for one iteration: the wait before its first (a
/// blocked task's return), then attempts until one delivers or no return
/// follows.
pub(super) struct Script {
    pub(super) initial: Follow,
    pub(super) steps: Vec<Step>,
    /// It was still retrying after [`MAX_ATTEMPTS`].
    pub(super) limited: bool,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            initial: Follow::Requeue(0.0),
            steps: Vec::new(),
            limited: false,
        }
    }
}

impl Script {
    /// Hours until it finishes, ignoring contention; `None` if it never does.
    pub(super) fn finish_hours(&self) -> Option<f64> {
        let mut total = match self.initial {
            Follow::Requeue(wait) => wait,
            Follow::Resolve(wait) => return Some(wait),
            Follow::Absorb(_) | Follow::Censored | Follow::Never => return None,
        };
        for step in &self.steps {
            total += step.hours;
            if step.outcome == EpisodeOutcome::Delivered {
                return Some(total);
            }
            match step.then {
                Follow::Requeue(wait) => total += wait,
                Follow::Resolve(wait) => return Some(total + wait),
                Follow::Absorb(_) | Follow::Censored | Follow::Never => return None,
            }
        }
        None
    }
}

/// Draw `item`'s attempts into `script`: each attempt samples an episode of
/// its outcome pool; one that did not deliver holds its slot for the
/// episode's hours and is followed by an immediate requeue (failed), a
/// sampled return (blocked) or nothing (abandoned). A delivering attempt
/// takes `delivery`. Without episodes the first attempt delivers.
fn script_attempts(
    rng: &mut Rng,
    item: &Item,
    resolved: &Resolved,
    episodes: &[Episode],
    delivery: Draw,
    script: &mut Script,
) {
    script.steps.clear();
    script.limited = false;
    script.initial = if item.blocked {
        resolved.returns.draw(rng, item.age_hours)
    } else {
        Follow::Requeue(0.0)
    };
    if !matches!(script.initial, Follow::Requeue(_)) {
        return;
    }
    for attempt in 0..MAX_ATTEMPTS {
        // The running attempt has already held its slot and used its spend.
        let running = attempt == 0 && item.in_progress;
        let episode = (!resolved.episodes.is_empty())
            .then(|| &episodes[resolved.episodes[rng.index(resolved.episodes.len())]])
            .filter(|episode| episode.outcome != EpisodeOutcome::Delivered);
        let Some(episode) = episode else {
            let spend = if running {
                delivery.less(item.spent)
            } else {
                delivery
            };
            script.steps.push(Step {
                hours: spend.hours.unwrap_or(0.0),
                spend,
                outcome: EpisodeOutcome::Delivered,
                then: Follow::Never,
            });
            return;
        };
        let redraw = |rng: &mut Rng, pool: &[usize], metric: fn(&Episode) -> Option<f64>| {
            (!pool.is_empty())
                .then(|| metric(&episodes[pool[rng.index(pool.len())]]))
                .flatten()
        };
        let tokens = match episode.tokens {
            Some(tokens) => Some(tokens),
            None => redraw(rng, &resolved.setback_tokens, |episode| episode.tokens),
        };
        let usd = match episode.usd {
            Some(usd) => Some(usd),
            None => redraw(rng, &resolved.setback_costs, |episode| episode.usd),
        };
        let mut spend = Draw {
            hours: Some(episode.hold_hours),
            tokens,
            usd,
        };
        if running {
            spend = spend.less(Spent {
                hours: item.age_hours,
                ..item.spent
            });
        }
        let then = match episode.outcome {
            EpisodeOutcome::Failed => Follow::Requeue(0.0),
            EpisodeOutcome::Blocked => resolved.returns.draw(rng, 0.0),
            EpisodeOutcome::Abandoned | EpisodeOutcome::Delivered => Follow::Never,
        };
        script.steps.push(Step {
            hours: spend.hours.unwrap_or(0.0),
            spend,
            outcome: episode.outcome,
            then,
        });
        if !matches!(then, Follow::Requeue(_)) {
            return;
        }
    }
    script.limited = true;
    if let Some(last) = script.steps.last_mut() {
        last.then = Follow::Never;
    }
}

fn draw(rng: &mut Rng, resolved: &Resolved, history: &[Delivery]) -> Draw {
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
        hours: Some(delivery.hours),
        tokens,
        usd,
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
    waits: [Vec<Option<f64>>; schedule::WAITS],
    /// Summed task-hours waiting to return after leaving the queue.
    return_waits: Vec<Option<f64>>,
    /// Over available iterations: attempts started that were blocked,
    /// failed or abandoned; tasks left without a return; iterations in
    /// which a task hit [`MAX_ATTEMPTS`].
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
        self.return_waits.push(run.return_wait);
        self.truncated += u64::from(run.truncated);
        if run.waits.is_none() {
            return;
        }
        self.available += 1;
        for (total, count) in self.setbacks.iter_mut().zip(run.setbacks) {
            *total += count;
        }
        self.no_return += run.no_return as u64;
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
        waits.insert("return".into(), quantiles(&mut self.return_waits));
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
            "not_expected_to_drain": full.then(|| mean(self.unfinished)).flatten(),
            // Means per simulated drain, over the scheduled tasks.
            "outcomes": {
                "blocked_attempts": mean(self.setbacks[0]),
                "failed_attempts": mean(self.setbacks[1]),
                "abandoned_attempts": mean(self.setbacks[2]),
                "no_return": mean(self.no_return),
                "attempt_limited": self.attempt_limited,
                "max_attempts": MAX_ATTEMPTS,
            },
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
#[allow(clippy::too_many_arguments)]
pub(super) fn simulate_backlog_forecast(
    items: &[Item],
    resolved: &[Resolved],
    history: &[Delivery],
    episodes: &[Episode],
    admission: &ForecastAdmission,
    plan: &DependencyPlan,
    slots: usize,
    arrivals: Option<&Arrivals<'_>>,
) -> Forecast {
    let mut rng = Rng(SEED);
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
            let delivery = draw(&mut rng, &resolved[index], history);
            hours[index] = delivery.hours;
            script_attempts(
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
            let mut totals = Draw::ZERO;
            let run = schedule::simulate_drain(
                scenario,
                &hours,
                &scripts,
                arrivals,
                &mut rng,
                &mut totals,
                &mut state,
            );
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
            outcome.record(scenario, &run, &state, totals);
            outcome.attempt_limited += u64::from(
                run.waits.is_some()
                    && (0..items.len())
                        .any(|index| scenario.included[index] && scripts[index].limited),
            );
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
