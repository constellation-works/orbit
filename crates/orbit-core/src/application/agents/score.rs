//! The one score every role uses. A role supplies its unit of work, its
//! terms and their weights, and one [`RowInput`] per crew and model; this
//! module turns each term's count into a rate per 100 units, sums the
//! weighted rates into the raw score (or 100 plus weighted deviations when
//! the role supplies expectations), pulls each row above the fleet average
//! down toward it in proportion to how little work backs it, and ranks the
//! rows that meet the minimum support.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::AgentRole;
use super::facts::TaskFacts;

/// Units of fleet-average evidence a row's score is blended with:
/// `score = min(raw, (units × raw + K × fleet_average) ÷ (units + K))`.
/// Blending only ever lowers a score: a row above the average is pulled down
/// toward it, and one below keeps its raw score, so a small row cannot rise
/// on the fleet's evidence or outrank a larger row with the same raw score.
pub(super) const SMOOTHING: f64 = 30.0;

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

/// One crew and model's work and term tallies in the window.
pub(super) struct RowInput<'a> {
    pub(super) crew: String,
    pub(super) model: String,
    /// The role's units of work in the window.
    pub(super) units: usize,
    pub(super) runs_without_model: usize,
    /// Reviews younger than the miss lookback; reviewers only.
    pub(super) provisional: Option<usize>,
    pub(super) tallies: BTreeMap<&'static str, Tally<'a>>,
    pub(super) stats: Vec<Stat>,
    /// Implementer reference counts for the row's tier mix; other roles
    /// retain their uncentred score when absent.
    pub(super) expected: Option<BTreeMap<&'static str, f64>>,
}

/// A row with its rates and scores.
pub(super) struct Scored<'a> {
    pub(super) input: RowInput<'a>,
    per_100: BTreeMap<&'static str, f64>,
    raw: f64,
    score: f64,
    below_support: bool,
    rank: Option<usize>,
}

/// Score and order `rows`: rows meeting `min_support` first, best score
/// first, each ranked; the rest after them without a rank. Also returns the
/// fleet average, the units-weighted mean raw score; `None` without units.
pub(super) fn score<'a>(
    terms: &[Term],
    rows: Vec<RowInput<'a>>,
    min_support: usize,
) -> (Option<f64>, Vec<Scored<'a>>) {
    let mut scored: Vec<Scored<'a>> = rows
        .into_iter()
        .map(|input| {
            let units = input.units.max(1) as f64;
            let per_100: BTreeMap<&'static str, f64> = terms
                .iter()
                .map(|term| {
                    let count = input.tallies.get(term.key).map_or(0.0, |tally| tally.count);
                    (term.key, count * 100.0 / units)
                })
                .collect();
            let raw = if input.expected.is_some() { 100.0 } else { 0.0 }
                + terms
                    .iter()
                    .map(|term| {
                        term.weight
                            * (per_100[term.key]
                                - input
                                    .expected
                                    .as_ref()
                                    .and_then(|expected| expected.get(term.key))
                                    .copied()
                                    .unwrap_or(0.0)
                                    * 100.0
                                    / units)
                    })
                    .sum::<f64>();
            Scored {
                below_support: input.units < min_support,
                input,
                per_100,
                raw,
                score: raw,
                rank: None,
            }
        })
        .collect();
    let units: f64 = scored.iter().map(|row| row.input.units as f64).sum();
    let fleet_average = (units > 0.0).then(|| {
        scored
            .iter()
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
        left.below_support
            .cmp(&right.below_support)
            .then(
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(Ordering::Equal),
            )
            .then(right.input.units.cmp(&left.input.units))
            .then(left.input.crew.cmp(&right.input.crew))
            .then(left.input.model.cmp(&right.input.model))
    });
    for (index, row) in scored
        .iter_mut()
        .filter(|row| !row.below_support)
        .enumerate()
    {
        row.rank = Some(index + 1);
    }
    (fleet_average, scored)
}

impl Scored<'_> {
    pub(super) fn is(&self, crew: &str, model: &str) -> bool {
        self.input.crew == crew && self.input.model == model
    }

    /// The ranking row.
    pub(super) fn row(&self, terms: &[Term]) -> Value {
        let counts: serde_json::Map<String, Value> = terms
            .iter()
            .map(|term| (term.key.to_string(), number(self.count(term.key))))
            .collect();
        let per_100: serde_json::Map<String, Value> = self
            .per_100
            .iter()
            .map(|(key, rate)| ((*key).to_string(), number(*rate)))
            .collect();
        let mut payload = json!({
            "crew": self.input.crew,
            "model": self.input.model,
            "units": self.input.units,
            "below_support": self.below_support,
            "rank": self.rank,
            "counts": counts,
            "per_100": per_100,
            "raw": number(self.raw),
            "score": number(self.score),
            "runs_without_model": self.input.runs_without_model,
            "provisional": self.input.provisional,
            "stats": self.input.stats.iter().map(|stat| json!({
                "key": stat.key,
                "label": stat.label,
                "value": stat.value,
            })).collect::<Vec<_>>(),
        });
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
        let expected = self
            .input
            .expected
            .as_ref()
            .and_then(|counts| counts.get(term.key))
            .copied()
            .unwrap_or(0.0);
        term.weight * (self.per_100[term.key] - expected * 100.0 / self.input.units.max(1) as f64)
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
                json!({
                    "key": term.key,
                    "label": term.label,
                    "weight": term.weight,
                    "group": term.group,
                    "count": number(self.count(term.key)),
                    "per_100": number(self.per_100[term.key]),
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
                })
            })
            .collect()
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

/// The role's terms as every response carries them.
pub(super) fn terms_json(terms: &[Term]) -> Vec<Value> {
    terms
        .iter()
        .map(|term| {
            json!({
                "key": term.key,
                "label": term.label,
                "weight": term.weight,
                "definition": term.definition,
                "group": term.group,
            })
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
