//! Per-metric, per-complexity correction learned from scored task residuals.
//!
//! The simulation resamples delivery history as if those draws were already
//! calibrated. This pass estimates how far that was from the truth for the
//! current model version and applies the estimate to the next forecast's
//! per-task draws. Whole-forecast totals are never training inputs: they
//! take up to 90 days to settle, and a forecast's own tasks are excluded
//! so its outcome cannot train it.
//!
//! Forgetting is exponential. A residual's weight is `0.5` raised to
//! `age_days / HALF_LIFE_DAYS`, and a residual older than [`WINDOW_DAYS`]
//! is dropped. The fit is a single pass over the stored forecasts scoring
//! already loaded. That set is bounded (the newest stored rows, plus the
//! mature window), so the pass is recomputed each forecast rather than
//! stored as an incremental baseline that would keep stale crew and effort
//! mix.
//!
//! A model version only learns from its own scored tasks. Each distinct
//! task contributes one forecast, and so one residual per metric: the first
//! eligible stored forecast taken while the task had not started
//! ([`scoring::TASK_POPULATION`], the population task-level calibration
//! scores). A forecast taken after work started predicts only what was
//! left, so it is not a residual. `n` is that task count, so hourly
//! snapshots of one task cannot meet the minimum on their own.
//!
//! A cohort that meets the minimum is checked out of sample before it is
//! applied: each kept task's base p90 is corrected by the cohort fitted
//! without it, and when that leave-one-out p90 coverage is below both the
//! base coverage and the 90% target the cohort is withheld. Bumping the version
//! starts the correction over. Rows written by this version store each
//! task's uncorrected quantiles under `forecast.base`; the fit reads those,
//! so a later forecast does not treat an already-corrected quantile as the
//! raw prediction. A row with no `base` object is an uncorrected prediction,
//! and its stored quantiles are the base.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use orbit_store::contracts::StoredBacklogForecast;
use orbit_types::task::{Task, TaskStatus, complexity_bucket};
use serde::Deserialize;
use serde_json::{Value, json};

use super::model::{self, Draw, Script};
use super::outcomes::TaskEpisodes;
use super::usage::ForecastRun;
use super::{scoring, usage};

/// Exponential half-life of a scored residual, in days.
pub(super) const HALF_LIFE_DAYS: f64 = 14.0;
/// Residuals older than this many days are dropped. The same bound is the
/// per-forecast scoring cutoff: a task done more than 90 days after its
/// forecast is not a settled score.
pub(super) const WINDOW_DAYS: f64 = 90.0;
/// Below this many distinct tasks a cohort is not corrected.
pub(super) const MIN_COHORT_N: usize = 8;
/// Largest absolute log adjustment applied to a draw, `ln(10)`.
const MAX_LOG_ADJUST: f64 = std::f64::consts::LN_10;
/// `ln(p90/p50)` below this is treated as no spread to rescale.
const MIN_LOG_RATIO: f64 = 0.05;
const METRICS: [&str; 3] = ["work_hours", "tokens", "usd"];

/// Coverage the p90 is meant to reach; a holdout below it and below the
/// base withholds the cohort.
const P90_TARGET: f64 = 0.9;
/// Relative slack when comparing an actual with a quantile, so a corrected
/// quantile that reproduces the actual through `ln`/`exp` still counts.
const HIT_TOLERANCE: f64 = 1e-9;

/// Why a cohort with residuals is not applied.
#[derive(Clone, Copy)]
enum Withheld {
    BelowMinN,
    P90Holdout,
}

impl Withheld {
    fn key(self) -> &'static str {
        match self {
            Self::BelowMinN => "below_min_n",
            Self::P90Holdout => "p90_holdout",
        }
    }
}

/// Leave-one-out p90 coverage of a cohort: how many held-out tasks fell at
/// or under the base p90, and under the p90 corrected without them.
#[derive(Clone, Copy)]
struct Holdout {
    n: u64,
    base_hits: u64,
    corrected_hits: u64,
}

impl Holdout {
    fn share(self, hits: u64) -> Option<f64> {
        (self.n > 0).then(|| hits as f64 / self.n as f64)
    }

    /// The corrected p90 covers less than the base and less than the target.
    fn lowers_p90(self) -> bool {
        match (self.share(self.corrected_hits), self.share(self.base_hits)) {
            (Some(corrected), Some(base)) => corrected < base && corrected < P90_TARGET,
            _ => false,
        }
    }

    fn to_json(self) -> Value {
        json!({
            "method": "leave_one_out",
            "n": self.n,
            "base_under_p90": self.share(self.base_hits).map(round3),
            "corrected_under_p90": self.share(self.corrected_hits).map(round3),
        })
    }
}

#[derive(Clone, Copy)]
struct Term {
    applied: bool,
    withheld: Option<Withheld>,
    holdout: Option<Holdout>,
    n: u64,
    weight: f64,
    bias: Option<f64>,
    p50_log_adjust: f64,
    p90_log_adjust: f64,
    spread: f64,
    center: f64,
    scale_ratio: Option<f64>,
}

impl Default for Term {
    fn default() -> Self {
        Self {
            applied: false,
            withheld: None,
            holdout: None,
            n: 0,
            weight: 0.0,
            bias: None,
            p50_log_adjust: 0.0,
            p90_log_adjust: 0.0,
            spread: 0.0,
            center: 1.0,
            scale_ratio: None,
        }
    }
}

impl Term {
    fn apply(self, value: f64) -> f64 {
        if !self.applied {
            return value;
        }
        self.map(value)
    }

    /// The log-affine map, whether or not the cohort is applied.
    fn map(self, value: f64) -> f64 {
        if value <= 0.0 || !value.is_finite() {
            return value;
        }
        let center = if self.center > 0.0 { self.center } else { 1.0 };
        let log_value = value.ln();
        let adjusted = log_value + self.p50_log_adjust + self.spread * (log_value - center.ln());
        let shift = (adjusted - log_value).clamp(-MAX_LOG_ADJUST, MAX_LOG_ADJUST);
        (log_value + shift).exp()
    }
}

/// The correction in use for this forecast. Cohorts below [`MIN_COHORT_N`]
/// are present and unapplied.
pub(super) struct Correction {
    /// Wall time of this fit, in milliseconds.
    pub(super) fit_ms: f64,
    by_complexity: BTreeMap<String, [Term; 3]>,
}

impl Correction {
    pub(super) fn to_json(&self) -> Value {
        let mut cohorts = Vec::new();
        for (complexity, terms) in &self.by_complexity {
            for (metric, term) in terms.iter().enumerate() {
                let estimated = term.n > 0;
                cohorts.push(json!({
                    "metric": METRICS[metric],
                    "complexity": complexity,
                    "n": term.n,
                    "weight": round3(term.weight),
                    "applied": term.applied,
                    "withheld": term.withheld.map(Withheld::key),
                    "holdout": term.holdout.map(Holdout::to_json),
                    "bias": term.bias.map(round3),
                    "p50_log_adjust": estimated.then(|| round3(term.p50_log_adjust)),
                    "p90_log_adjust": estimated.then(|| round3(term.p90_log_adjust)),
                    "spread": estimated.then(|| round3(term.spread)),
                    "center": estimated.then(|| round3(term.center)),
                    "scale_ratio": term.scale_ratio.map(round3),
                }));
            }
        }
        json!({
            "half_life_days": HALF_LIFE_DAYS,
            "window_days": WINDOW_DAYS,
            "min_n": MIN_COHORT_N,
            "max_abs_log_adjust": round3(MAX_LOG_ADJUST),
            "population": scoring::TASK_POPULATION,
            "p90_holdout_target": P90_TARGET,
            "cohorts": cohorts,
        })
    }

    fn apply_metric(&self, metric: usize, complexity: &str, value: f64) -> f64 {
        self.by_complexity
            .get(complexity)
            .map_or(value, |terms| terms[metric].apply(value))
    }
}

/// Inputs the fit reads. `items` are the tasks this forecast is about;
/// none of them train it.
pub(super) struct CorrectionInputs<'a> {
    pub(super) stored: &'a [StoredBacklogForecast],
    pub(super) tasks: &'a HashMap<String, Task>,
    pub(super) done_at: &'a HashMap<String, DateTime<Utc>>,
    pub(super) runs: &'a HashMap<String, Vec<ForecastRun>>,
    pub(super) episodes: &'a HashMap<String, TaskEpisodes>,
    pub(super) items: &'a [model::Item],
    pub(super) now: DateTime<Utc>,
}

#[derive(Deserialize)]
struct StoredTask {
    id: String,
    complexity: String,
    #[serde(default)]
    in_progress: bool,
    #[serde(default)]
    forecast: Value,
}

struct Sample {
    actual: f64,
    e50: f64,
    e90: Option<f64>,
    base_p50: f64,
    base_p90: Option<f64>,
    ratio: Option<f64>,
    weight: f64,
}

/// Fit the correction and record how long the pass took.
pub(super) fn fit_correction(inputs: &CorrectionInputs<'_>) -> Correction {
    let started = std::time::Instant::now();
    let cohort_ids: HashSet<&str> = inputs
        .items
        .iter()
        .map(|item| item.task_id.as_str())
        .collect();
    let mut complexities: BTreeSet<String> = inputs
        .items
        .iter()
        .map(|item| item.complexity.clone())
        .collect();
    // One forecast per (complexity, task): its first eligible stored
    // forecast taken before work started. An earlier one replaces a later
    // one; the same timestamp keeps the first read.
    type Kept = (DateTime<Utc>, [Option<Sample>; 3]);
    let mut kept: BTreeMap<(String, String), Kept> = BTreeMap::new();
    for forecast in inputs.stored {
        if forecast.model_version != model::MODEL_VERSION || forecast.forecast_at >= inputs.now {
            continue;
        }
        let stored_tasks: Vec<StoredTask> =
            serde_json::from_str(&forecast.tasks_json).unwrap_or_default();
        for stored_task in stored_tasks {
            if stored_task.in_progress {
                continue;
            }
            let key = (stored_task.complexity.clone(), stored_task.id.clone());
            if kept
                .get(&key)
                .is_some_and(|(forecast_at, _)| *forecast_at <= forecast.forecast_at)
            {
                continue;
            }
            let Some(sample_at) = eligible_done(inputs, forecast, &stored_task, &cohort_ids) else {
                continue;
            };
            let Some(actual) = task_actual(inputs, forecast, &stored_task.id, sample_at) else {
                continue;
            };
            let Some(weight) = residual_weight(sample_at, inputs.now) else {
                continue;
            };
            let actuals = [Some(actual.0), actual.1, actual.2];
            let mut samples: [Option<Sample>; 3] = [None, None, None];
            for (metric, actual) in actuals.into_iter().enumerate() {
                samples[metric] = residual_sample(&stored_task.forecast, metric, actual, weight);
            }
            if samples.iter().all(Option::is_none) {
                continue;
            }
            complexities.insert(key.0.clone());
            kept.insert(key, (forecast.forecast_at, samples));
        }
    }
    let mut samples: BTreeMap<(usize, String), Vec<Sample>> = BTreeMap::new();
    for ((complexity, _), (_, metrics)) in kept {
        for (metric, sample) in metrics.into_iter().enumerate() {
            if let Some(sample) = sample {
                samples
                    .entry((metric, complexity.clone()))
                    .or_default()
                    .push(sample);
            }
        }
    }
    let mut by_complexity: BTreeMap<String, [Term; 3]> = complexities
        .iter()
        .map(|complexity| (complexity.clone(), [Term::default(); 3]))
        .collect();
    for ((metric, complexity), cohort) in samples {
        if let Some(terms) = by_complexity.get_mut(&complexity) {
            terms[metric] = fit_term(&cohort);
        }
    }
    Correction {
        fit_ms: started.elapsed().as_secs_f64() * 1000.0,
        by_complexity,
    }
}

/// One metric's residual against the stored base, when both sides are
/// positive numbers.
fn residual_sample(
    forecast: &Value,
    metric: usize,
    actual: Option<f64>,
    weight: f64,
) -> Option<Sample> {
    let actual = actual.filter(|value| value.is_finite() && *value > 0.0)?;
    let (base_p50, base_p90) = base_quantile(forecast, METRICS[metric])?;
    let e50 = (actual / base_p50).ln();
    if !e50.is_finite() {
        return None;
    }
    let e90 = base_p90
        .map(|p90| (actual / p90).ln())
        .filter(|e| e.is_finite());
    let ratio = base_p90
        .filter(|p90| *p90 > base_p50)
        .map(|p90| p90 / base_p50);
    Some(Sample {
        actual,
        e50,
        e90,
        base_p50,
        base_p90,
        ratio,
        weight,
    })
}

/// Sum of one uncorrected script's spend. The schedule may charge only the
/// attempts it starts; for a delivery that finishes inside the horizon the
/// two match, and the stored base quantiles are this draw's distribution.
pub(super) fn script_spend(script: &Script) -> Draw {
    let mut total = Draw::ZERO;
    for step in &script.steps {
        total.add(&step.spend);
    }
    total
}

/// Apply the cohort correction to the script as a whole.
///
/// The fit's residuals are per full task: `base_p50`, `center` and the
/// actual are all full-script totals (see [`script_spend`]). Applying the
/// log-affine map to each step instead would correct a different unit
/// whenever `spread != 0`: the sum of corrected steps would not be the
/// correction of the summed script the residuals were trained on. So the
/// map is applied once per metric to the script's uncorrected total, and
/// the steps are scaled by `corrected_total / total` so their corrected
/// sum equals that corrected total (within floating-point rounding).
/// A single-step script therefore keeps its previous behaviour, and a
/// step with a missing metric stays missing: the total for a metric is
/// the sum of the steps that carry it.
pub(super) fn apply_script(correction: &Correction, complexity: &str, script: &mut Script) {
    if script.steps.is_empty() {
        return;
    }
    let present_total = |metric: usize| -> Option<f64> {
        let mut total = 0.0;
        let mut present = false;
        for step in &script.steps {
            let value = match metric {
                0 => step.spend.hours,
                1 => step.spend.tokens,
                _ => step.spend.usd,
            };
            if let Some(value) = value {
                present = true;
                total += value;
            }
        }
        present.then_some(total)
    };
    let factor = |metric: usize| -> f64 {
        let Some(total) = present_total(metric) else {
            return 1.0;
        };
        if !total.is_finite() || total <= 0.0 {
            return 1.0;
        }
        let corrected = correction.apply_metric(metric, complexity, total);
        if !corrected.is_finite() {
            return 1.0;
        }
        corrected / total
    };
    let factors = [factor(0), factor(1), factor(2)];
    for step in &mut script.steps {
        if let Some(hours) = step.spend.hours {
            step.spend.hours = Some(hours * factors[0]);
            step.hours *= factors[0];
        }
        if let Some(tokens) = step.spend.tokens {
            step.spend.tokens = Some(tokens * factors[1]);
        }
        if let Some(usd) = step.spend.usd {
            step.spend.usd = Some(usd * factors[2]);
        }
    }
}

/// Fit one cohort. `samples` holds one residual per distinct task, so `n`
/// is that count and the cohort applies only at [`MIN_COHORT_N`] tasks,
/// and only when its leave-one-out p90 coverage does not fall below both
/// the base and [`P90_TARGET`].
fn fit_term(samples: &[Sample]) -> Term {
    let refs: Vec<&Sample> = samples.iter().collect();
    let mut term = estimate_term(&refs);
    if term.n == 0 {
        return term;
    }
    if samples.len() < MIN_COHORT_N {
        term.withheld = Some(Withheld::BelowMinN);
        return term;
    }
    let holdout = leave_one_out_p90(samples);
    term.holdout = holdout;
    if holdout.is_some_and(Holdout::lowers_p90) {
        term.withheld = Some(Withheld::P90Holdout);
    } else {
        term.applied = true;
    }
    term
}

/// Each task whose base p90 is known, scored against the base p90 and
/// against that p90 corrected by the cohort fitted without it.
fn leave_one_out_p90(samples: &[Sample]) -> Option<Holdout> {
    let mut holdout = Holdout {
        n: 0,
        base_hits: 0,
        corrected_hits: 0,
    };
    for (index, sample) in samples.iter().enumerate() {
        let Some(base_p90) = sample.base_p90 else {
            continue;
        };
        let rest: Vec<&Sample> = samples
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, other)| other)
            .collect();
        let without = estimate_term(&rest);
        if without.n == 0 {
            continue;
        }
        let hit = |p90: f64| sample.actual <= p90 * (1.0 + HIT_TOLERANCE);
        holdout.n += 1;
        holdout.base_hits += u64::from(hit(base_p90));
        holdout.corrected_hits += u64::from(hit(without.map(base_p90)));
    }
    (holdout.n > 0).then_some(holdout)
}

/// The cohort's terms, unapplied.
fn estimate_term(samples: &[&Sample]) -> Term {
    let weight: f64 = samples.iter().map(|sample| sample.weight).sum();
    if samples.is_empty() || weight <= 0.0 {
        return Term::default();
    }
    let bias = samples
        .iter()
        .map(|sample| sample.weight * sample.e50)
        .sum::<f64>()
        / weight;
    let e50: Vec<(f64, f64)> = samples
        .iter()
        .map(|sample| (sample.e50, sample.weight))
        .collect();
    let Some(q50) = weighted_quantile(&e50, 0.5) else {
        return Term::default();
    };
    let e90: Vec<(f64, f64)> = samples
        .iter()
        .filter_map(|sample| sample.e90.map(|value| (value, sample.weight)))
        .collect();
    let q90 = weighted_quantile(&e90, 0.9).unwrap_or(q50);
    let centers: Vec<(f64, f64)> = samples
        .iter()
        .map(|sample| (sample.base_p50, sample.weight))
        .collect();
    let center = weighted_quantile(&centers, 0.5).unwrap_or(1.0);
    let ratios: Vec<(f64, f64)> = samples
        .iter()
        .filter_map(|sample| sample.ratio.map(|ratio| (ratio, sample.weight)))
        .collect();
    let scale_ratio = weighted_quantile(&ratios, 0.5);
    let q50 = q50.clamp(-MAX_LOG_ADJUST, MAX_LOG_ADJUST);
    let q90 = q90.clamp(-MAX_LOG_ADJUST, MAX_LOG_ADJUST);
    let spread = scale_ratio
        .filter(|ratio| ratio.ln() > MIN_LOG_RATIO)
        .map_or(0.0, |ratio| (q90 - q50) / ratio.ln());
    Term {
        applied: false,
        withheld: None,
        holdout: None,
        n: samples.len() as u64,
        weight,
        bias: bias.is_finite().then_some(bias),
        p50_log_adjust: q50,
        p90_log_adjust: q90,
        spread,
        center,
        scale_ratio,
    }
}

/// Smallest value whose cumulative weight reaches `q` of the total.
fn weighted_quantile(samples: &[(f64, f64)], q: f64) -> Option<f64> {
    let mut rows: Vec<(f64, f64)> = samples
        .iter()
        .copied()
        .filter(|(value, weight)| value.is_finite() && *weight > 0.0)
        .collect();
    if rows.is_empty() {
        return None;
    }
    rows.sort_by(|left, right| left.0.total_cmp(&right.0));
    let total: f64 = rows.iter().map(|(_, weight)| weight).sum();
    if total <= 0.0 {
        return None;
    }
    let target = q.clamp(0.0, 1.0) * total;
    let mut covered = 0.0;
    let mut last = None;
    for (value, weight) in rows {
        covered += weight;
        last = Some(value);
        if covered + f64::EPSILON >= target {
            return Some(value);
        }
    }
    last
}

fn eligible_done(
    inputs: &CorrectionInputs<'_>,
    forecast: &StoredBacklogForecast,
    stored_task: &StoredTask,
    cohort_ids: &HashSet<&str>,
) -> Option<DateTime<Utc>> {
    if cohort_ids.contains(stored_task.id.as_str()) {
        return None;
    }
    let task = inputs.tasks.get(&stored_task.id)?;
    if task.status != TaskStatus::Done {
        return None;
    }
    if complexity_bucket(task.complexity.map(|c| c.as_str())) != stored_task.complexity {
        return None;
    }
    let done = *inputs.done_at.get(&stored_task.id)?;
    let settled = done > forecast.forecast_at
        && done <= inputs.now
        && done <= forecast.forecast_at + Duration::days(WINDOW_DAYS as i64);
    settled.then_some(done)
}

fn task_actual(
    inputs: &CorrectionInputs<'_>,
    forecast: &StoredBacklogForecast,
    task_id: &str,
    done: DateTime<Utc>,
) -> Option<(f64, Option<f64>, Option<f64>)> {
    let runs = inputs.runs.get(task_id).map_or(&[][..], Vec::as_slice);
    let used = usage::forecast_usage(runs, Some(forecast.forecast_at));
    if used.invocations == 0 {
        return None;
    }
    let hours = scoring::attempt_work_hours(
        runs,
        inputs.episodes.get(task_id),
        forecast.forecast_at,
        Some(done),
    );
    (hours > 0.0).then_some((hours, used.tokens, used.usd))
}

fn residual_weight(done: DateTime<Utc>, now: DateTime<Utc>) -> Option<f64> {
    let age_secs = (now - done).num_seconds();
    if age_secs < 0 {
        return None;
    }
    let age_days = age_secs as f64 / 86_400.0;
    (age_days <= WINDOW_DAYS).then(|| 0.5_f64.powf(age_days / HALF_LIFE_DAYS))
}

/// Uncorrected `(p50, p90)` for one metric. `p90` is absent when it was
/// not a positive number. A stored `base` object is authoritative; without
/// one the stored quantile itself is the uncorrected prediction.
fn base_quantile(forecast: &Value, key: &str) -> Option<(f64, Option<f64>)> {
    let node = if let Some(base) = forecast.get("base").filter(|base| base.is_object()) {
        base.get(key)?
    } else {
        forecast.get(key)?
    };
    let p50 = node.get("p50")?.as_f64()?;
    if !p50.is_finite() || p50 <= 0.0 {
        return None;
    }
    let p90 = node
        .get("p90")
        .and_then(Value::as_f64)
        .filter(|p90| p90.is_finite() && *p90 > 0.0);
    Some((p50, p90))
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}
