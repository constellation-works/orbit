//! The one score every role uses. A role supplies its unit of work, its
//! terms and their weights, and one [`RowInput`] per crew and model; this
//! module turns each term's count into a rate per 100 units (or per 100 of
//! the term's own [`Denominator`]), sums the weighted rates into the raw
//! score (or 100 plus weighted deviations when the role supplies
//! expectations), pulls each row above the fleet average down toward it in
//! proportion to how little work backs it, and ranks the rows that meet the
//! minimum support. A row none of whose units is judged yet has no score
//! and no rank, and stays out of the fleet average.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::AgentRole;
use super::facts::TaskFacts;
use super::rank::sample::{Rng, percentile};

/// Units of fleet-average evidence a row's score is blended with:
/// `score = min(raw, (units × raw + K × fleet_average) ÷ (units + K))`.
/// Blending only ever lowers a score: a row above the average is pulled down
/// toward it, and one below keeps its raw score, so a small row cannot rise
/// on the fleet's evidence or outrank a larger row with the same raw score.
pub(super) const SMOOTHING: f64 = 30.0;

/// Reviewed deliveries an implementer row, by-type cell or ranked model needs
/// before its review terms score. Below it the terms are provisional: rated
/// and shown, but left out of the raw score (or, on the model ranking, the
/// review-outcome measure is unmeasured and leaves the weight denominator).
/// Zero reviewed deliveries stays unmeasured. Matches [`SMOOTHING`]; one named
/// value for every surface, returned as `review_min_support`.
pub(super) const REVIEW_MIN_SUPPORT: usize = 30;

/// Resamples per ranked row's interval, and the seed every row's resample
/// starts from, so the same facts give the same interval on every request.
const INTERVAL_DRAWS: usize = 1000;
const INTERVAL_SEED: u64 = 0x7363_6f72_6563_6976;

/// One weighted term of a role's score.
pub(super) struct Term {
    pub(super) key: &'static str,
    pub(super) label: &'static str,
    /// Positive weights are credit; negative weights are problems.
    pub(super) weight: f64,
    pub(super) definition: &'static str,
    /// `credit` or `problem`.
    pub(super) group: &'static str,
}

/// One record behind a task's count.
pub(super) struct Occurrence {
    pub(super) at: DateTime<Utc>,
    pub(super) detail: String,
    pub(super) run_id: Option<String>,
}

/// A task's part of one term's count and the records behind it.
pub(super) struct TaskTally<'a> {
    task: &'a TaskFacts,
    count: f64,
    occurrences: Vec<Occurrence>,
}

/// One term's count on a row and the tasks behind it.
#[derive(Default)]
pub(super) struct Tally<'a> {
    count: f64,
    tasks: BTreeMap<&'a str, TaskTally<'a>>,
}

impl<'a> Tally<'a> {
    /// The term's total count on the row or cell.
    pub(super) fn count(&self) -> f64 {
        self.count
    }

    /// Actual counts by task, for tier reference rates without rescanning.
    pub(super) fn task_counts(&self) -> impl Iterator<Item = (&TaskFacts, f64)> {
        self.tasks.values().map(|tally| (tally.task, tally.count))
    }

    /// Each task behind the count with its count and the records behind it.
    pub(super) fn task_records(&self) -> impl Iterator<Item = (&TaskFacts, f64, &[Occurrence])> {
        self.tasks
            .values()
            .map(|tally| (tally.task, tally.count, tally.occurrences.as_slice()))
    }

    pub(super) fn add(&mut self, task: &'a TaskFacts, count: f64, occurrence: Option<Occurrence>) {
        self.count += count;
        let entry = self
            .tasks
            .entry(task.id.as_str())
            .or_insert_with(|| TaskTally {
                task,
                count: 0.0,
                occurrences: Vec::new(),
            });
        entry.count += count;
        entry.occurrences.extend(occurrence);
    }
}

/// Context shown beside a row and never scored.
pub(super) struct Stat {
    pub(super) key: &'static str,
    pub(super) label: &'static str,
    pub(super) value: Value,
}

/// A term rated per 100 of its own units rather than the row's.
pub(super) struct Denominator {
    pub(super) units: usize,
    /// Below this many units the term is provisional: rated and shown, but
    /// left out of the raw score.
    pub(super) min_support: usize,
}

impl Denominator {
    fn provisional(&self) -> bool {
        self.units < self.min_support
    }
}

/// One model-and-recorded-effort row's work and term tallies in the
/// window. `crew` is the first contributing crew, kept so a detail
/// selection by crew still resolves; `crews` lists every crew whose units
/// the row groups.
pub(super) struct RowInput<'a> {
    pub(super) crew: String,
    pub(super) crews: Vec<String>,
    pub(super) model: String,
    /// The row's recorded effort in the shared encoding (`None` is the
    /// provider default, `Some("unknown")` is unrecorded historical work).
    pub(super) effort: super::effort::RecordedEffort,
    /// The role's units of work in the window.
    pub(super) units: usize,
    pub(super) runs_without_model: usize,
    /// Reviews younger than the miss lookback; reviewers only.
    pub(super) provisional: Option<usize>,
    pub(super) tallies: BTreeMap<&'static str, Tally<'a>>,
    pub(super) stats: Vec<Stat>,
    /// Whether an implementer escape expectation used a broader reference.
    pub(super) escaped_regressions_fallback: Option<bool>,
    /// Implementer reference counts for the row's tier mix; other roles
    /// retain their uncentred score when absent.
    pub(super) expected: Option<BTreeMap<&'static str, f64>>,
    /// Terms rated per 100 of their own units; every other term is rated
    /// per 100 of `units`.
    pub(super) denominators: BTreeMap<&'static str, Denominator>,
    /// Tasks with a settled review behind them (implementers only):
    /// the population of the review terms' rates and expectations.
    /// Zero means unmeasured, with null rates and neutral contributions.
    pub(super) reviewed_units: Option<usize>,
    /// No unit of the row has been judged yet (reviewers only): it gets no
    /// score or rank, is listed last, and stays out of the fleet average.
    pub(super) unjudged: bool,
}

impl RowInput<'_> {
    /// The units `key` is rated per 100 of.
    fn units_of(&self, key: &str) -> usize {
        self.denominators
            .get(key)
            .map_or(self.units, |denominator| denominator.units)
    }

    /// Whether `key` is below its own minimum support.
    fn provisional_term(&self, key: &str) -> bool {
        self.denominators
            .get(key)
            .is_some_and(Denominator::provisional)
    }
}

/// A row with its rates and scores.
pub(super) struct Scored<'a> {
    pub(super) input: RowInput<'a>,
    per_100: BTreeMap<&'static str, f64>,
    raw: f64,
    score: f64,
    below_support: bool,
    rank: Option<usize>,
    /// The 10th and 90th percentiles of the row's resampled score; set only
    /// on ranked rows.
    interval: Option<(f64, f64)>,
}

/// Score and order `rows`: rows meeting `min_support` first, best score
/// first, each ranked; then the rest without a rank, and unjudged rows last.
/// Also returns the fleet average, the units-weighted mean raw score of the
/// judged rows; `None` without judged units.
pub(super) fn score<'a>(
    terms: &[Term],
    rows: Vec<RowInput<'a>>,
    min_support: usize,
) -> (Option<f64>, Vec<Scored<'a>>) {
    let mut scored: Vec<Scored<'a>> = rows
        .into_iter()
        .map(|input| {
            let per_100: BTreeMap<&'static str, f64> = terms
                .iter()
                .map(|term| {
                    let count = input.tallies.get(term.key).map_or(0.0, |tally| tally.count);
                    (
                        term.key,
                        count * 100.0 / input.units_of(term.key).max(1) as f64,
                    )
                })
                .collect();
            let raw = if input.expected.is_some() { 100.0 } else { 0.0 }
                + terms
                    .iter()
                    .map(|term| contribution(&input, &per_100, term))
                    .sum::<f64>();
            Scored {
                below_support: input.units < min_support,
                input,
                per_100,
                raw,
                score: raw,
                rank: None,
                interval: None,
            }
        })
        .collect();
    let judged = || scored.iter().filter(|row| !row.input.unjudged);
    let units: f64 = judged().map(|row| row.input.units as f64).sum();
    let fleet_average = (units > 0.0).then(|| {
        judged()
            .map(|row| row.input.units as f64 * row.raw)
            .sum::<f64>()
            / units
    });
    if let Some(average) = fleet_average {
        for row in &mut scored {
            let units = row.input.units as f64;
            let blended = (units * row.raw + SMOOTHING * average) / (units + SMOOTHING);
            row.score = row.raw.min(blended);
        }
    }
    scored.sort_by(|left, right| {
        left.input
            .unjudged
            .cmp(&right.input.unjudged)
            .then(left.below_support.cmp(&right.below_support))
            .then(
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(Ordering::Equal),
            )
            .then(right.input.units.cmp(&left.input.units))
            .then(left.input.model.cmp(&right.input.model))
            .then(
                super::effort::effort_label(&left.input.effort)
                    .cmp(&super::effort::effort_label(&right.input.effort)),
            )
            .then(left.input.crew.cmp(&right.input.crew))
    });
    for (index, row) in scored
        .iter_mut()
        .filter(|row| !row.below_support && !row.input.unjudged)
        .enumerate()
    {
        row.rank = Some(index + 1);
    }
    if let Some(average) = fleet_average {
        for row in scored.iter_mut().filter(|row| row.rank.is_some()) {
            row.interval = resampled_interval(terms, &row.input, average);
        }
    }
    (fleet_average, scored)
}

/// A ranked row's interval: its own units are resampled with replacement,
/// each draw rescored with the expectations, term denominators, smoothing
/// and fleet average held as they are, and the 10th and 90th percentiles of
/// the draws bound it. A row whose units are all unjudged or below support
/// has none, and so does a row with no units.
fn resampled_interval(
    terms: &[Term],
    input: &RowInput<'_>,
    fleet_average: f64,
) -> Option<(f64, f64)> {
    if input.units == 0 {
        return None;
    }
    // One vector of term counts per unit: each task with a tally, then zero
    // units for the rest of the row's units.
    let mut per_task: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for (index, term) in terms.iter().enumerate() {
        if let Some(tally) = input.tallies.get(term.key) {
            for (task, count) in tally.task_counts() {
                per_task
                    .entry(task.id.as_str())
                    .or_insert_with(|| vec![0.0; terms.len()])[index] += count;
            }
        }
    }
    let zero_units = input.units.saturating_sub(per_task.len());
    let mut pool: Vec<Vec<f64>> = per_task.into_values().collect();
    pool.extend(std::iter::repeat_with(|| vec![0.0; terms.len()]).take(zero_units));

    let units = input.units as f64;
    let mut rng = Rng::seeded(INTERVAL_SEED);
    let mut sums = vec![0.0; terms.len()];
    let mut scores = Vec::with_capacity(INTERVAL_DRAWS);
    for _ in 0..INTERVAL_DRAWS {
        sums.iter_mut().for_each(|sum| *sum = 0.0);
        for _ in 0..input.units {
            let unit = &pool[rng.below(pool.len())];
            for (sum, count) in sums.iter_mut().zip(unit) {
                *sum += count;
            }
        }
        let per_100: BTreeMap<&'static str, f64> = terms
            .iter()
            .zip(&sums)
            .map(|(term, sum)| {
                (
                    term.key,
                    sum * 100.0 / input.units_of(term.key).max(1) as f64,
                )
            })
            .collect();
        let raw = if input.expected.is_some() { 100.0 } else { 0.0 }
            + terms
                .iter()
                .map(|term| contribution(input, &per_100, term))
                .sum::<f64>();
        let blended = (units * raw + SMOOTHING * fleet_average) / (units + SMOOTHING);
        scores.push(raw.min(blended));
    }
    Some((
        percentile(&mut scores, 10.0)?,
        percentile(&mut scores, 90.0)?,
    ))
}

/// A term's part of the raw score: weight × (rate − expected rate), or 0
/// while the term has no units or is below its own minimum support.
fn contribution(input: &RowInput<'_>, per_100: &BTreeMap<&'static str, f64>, term: &Term) -> f64 {
    if input.provisional_term(term.key) || input.units_of(term.key) == 0 {
        return 0.0;
    }
    let expected = input
        .expected
        .as_ref()
        .and_then(|counts| counts.get(term.key))
        .copied()
        .unwrap_or(0.0);
    term.weight * (per_100[term.key] - expected * 100.0 / input.units_of(term.key).max(1) as f64)
}

impl Scored<'_> {
    /// Whether `crew` contributed units to this row.
    pub(super) fn has_crew(&self, crew: &str) -> bool {
        self.input.crew == crew || self.input.crews.iter().any(|name| name == crew)
    }

    /// The row's `model @ effort` label.
    pub(super) fn label(&self) -> String {
        super::effort::row_label(&self.input.model, &self.input.effort)
    }

    /// The ranking row. An unjudged row's `raw` and `score` are null.
    pub(super) fn row(&self, terms: &[Term]) -> Value {
        let counts: serde_json::Map<String, Value> = terms
            .iter()
            .map(|term| (term.key.to_string(), number(self.count(term.key))))
            .collect();
        let per_100: serde_json::Map<String, Value> = self
            .per_100
            .iter()
            .map(|(key, rate)| ((*key).to_string(), self.measured_rate(key, *rate)))
            .collect();
        let mut payload = json!({
            "crew": self.input.crew,
            "crews": self.input.crews,
            "model": self.input.model,
            "reasoning_effort": self.input.effort,
            "effort_label": super::effort::effort_label(&self.input.effort),
            "label": self.label(),
            "units": self.input.units,
            "below_support": self.below_support,
            "unjudged": self.input.unjudged,
            "rank": self.rank,
            "counts": counts,
            "per_100": per_100,
            "raw": (!self.input.unjudged).then(|| number(self.raw)),
            "score": (!self.input.unjudged).then(|| number(self.score)),
            "interval": self.interval.map(|(low, high)| json!({
                "p10": number(low),
                "p90": number(high),
            })),
            "runs_without_model": self.input.runs_without_model,
            "provisional": self.input.provisional,
            "stats": self.input.stats.iter().map(|stat| json!({
                "key": stat.key,
                "label": stat.label,
                "value": stat.value,
            })).collect::<Vec<_>>(),
        });
        if let Some(fallback) = self.input.escaped_regressions_fallback {
            payload["escaped_regressions_fallback"] = json!(fallback);
        }
        if !self.input.denominators.is_empty() {
            payload["term_units"] = json!(
                self.input
                    .denominators
                    .iter()
                    .map(|(key, denominator)| ((*key).to_string(), json!(denominator.units)))
                    .collect::<serde_json::Map<String, Value>>()
            );
            payload["provisional_terms"] = json!(
                self.input
                    .denominators
                    .iter()
                    .filter(|(_, denominator)| denominator.provisional())
                    .map(|(key, _)| *key)
                    .collect::<Vec<_>>()
            );
        }
        if let Some(reviewed) = self.input.reviewed_units {
            payload["reviewed_units"] = json!(reviewed);
        }
        if let Some(expected) = &self.input.expected {
            payload["expected"] = json!(
                expected
                    .iter()
                    .map(|(key, count)| ((*key).to_string(), number(*count)))
                    .collect::<serde_json::Map<String, Value>>()
            );
            payload["score_base"] = json!(100);
        }
        payload
    }

    fn contribution(&self, term: &Term) -> f64 {
        contribution(&self.input, &self.per_100, term)
    }

    /// Each term's count, rate and contribution to the raw score, with the
    /// tasks behind the count, largest first.
    pub(super) fn terms(&self, terms: &[Term]) -> Vec<Value> {
        terms
            .iter()
            .map(|term| {
                let mut tasks: Vec<&TaskTally<'_>> = self
                    .input
                    .tallies
                    .get(term.key)
                    .map(|tally| tally.tasks.values().collect())
                    .unwrap_or_default();
                tasks.sort_by(|left, right| {
                    right
                        .count
                        .abs()
                        .partial_cmp(&left.count.abs())
                        .unwrap_or(Ordering::Equal)
                        .then(right.task.created_at.cmp(&left.task.created_at))
                });
                let mut payload = json!({
                    "key": term.key,
                    "label": term.label,
                    "weight": term.weight,
                    "group": term.group,
                    "count": number(self.count(term.key)),
                    "per_100": self.measured_rate(term.key, self.per_100[term.key]),
                    "contribution": number(self.contribution(term)),
                    "tasks": tasks.iter().map(|tally| json!({
                        "task_id": tally.task.id,
                        "title": tally.task.title,
                        "status": tally.task.status.to_string(),
                        "count": number(tally.count),
                        "occurrences": tally.occurrences.iter().map(|occurrence| json!({
                            "at": occurrence.at.to_rfc3339(),
                            "detail": occurrence.detail,
                            "run_id": occurrence.run_id,
                        })).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                });
                if let Some(denominator) = self.input.denominators.get(term.key) {
                    payload["units"] = json!(denominator.units);
                    payload["provisional"] = json!(denominator.provisional());
                }
                payload
            })
            .collect()
    }

    fn measured_rate(&self, key: &str, rate: f64) -> Value {
        if self.input.reviewed_units == Some(0) && self.input.denominators.contains_key(key) {
            Value::Null
        } else {
            number(rate)
        }
    }

    fn count(&self, key: &str) -> f64 {
        self.input.tallies.get(key).map_or(0.0, |tally| tally.count)
    }
}

/// The window's provisional units, the sum of the rows' counts. Only
/// reviewers keep them; `None` for any other role.
pub(super) fn provisional(role: AgentRole, rows: &[Scored<'_>]) -> Option<usize> {
    (role == AgentRole::Reviewers)
        .then(|| rows.iter().filter_map(|row| row.input.provisional).sum())
}

/// The role's terms as every response carries them. A term in `units`,
/// rated per 100 of its own denominator, also names that unit.
pub(super) fn terms_json(terms: &[Term], units: &[(&str, &str)]) -> Vec<Value> {
    terms
        .iter()
        .map(|term| {
            let mut payload = json!({
                "key": term.key,
                "label": term.label,
                "weight": term.weight,
                "definition": term.definition,
                "group": term.group,
            });
            if let Some((_, unit)) = units.iter().find(|(key, _)| *key == term.key) {
                payload["unit"] = json!(unit);
            }
            payload
        })
        .collect()
}

/// A whole number as an integer, anything else rounded to two places.
pub(super) fn number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        json!(value as i64)
    } else {
        json!((value * 100.0).round() / 100.0)
    }
}
