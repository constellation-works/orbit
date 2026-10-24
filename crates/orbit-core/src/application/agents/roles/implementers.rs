//! Implementers: the crew and model of the implementation runs that did the
//! work. Every term is compared with the fleet on the same tier mix;
//! recorded cost and duration remain unscored context.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_types::workflow::ReviewVerdict;
use serde_json::{Value, json};

use super::RoleSpec;
use crate::application::agents::attribution::{Charge, Credit, Owner, row_key};
use crate::application::agents::facts::{Attempt, Problem, Review, TaskFacts};
use crate::application::agents::score::{Occurrence, RowInput, Stat, Tally, Term, number};
use crate::application::agents::{AgentFacts, AgentRole};

pub(super) const ROLE: RoleSpec = RoleSpec {
    unit: "implemented tasks",
    terms: TERMS,
    rows: Some(rows),
};

const TERMS: &[Term] = &[
    Term {
        key: "weighted_landed",
        label: "Landed",
        weight: 1.0,
        definition: "Landed tasks whose final implementation run is the row's, one point each, whatever their complexity.",
        group: "credit",
    },
    Term {
        key: "first_pass_accepts",
        label: "First-pass accepts",
        weight: 1.0,
        definition: "Reviewed tasks whose first review verdict on the row's candidate was accept.",
        group: "credit",
    },
    Term {
        key: "review_rejects",
        label: "Review rejects",
        weight: -1.0,
        definition: "reject verdicts on the row's candidates.",
        group: "problem",
    },
    Term {
        key: "reviewer_repairs",
        label: "Reviewer repairs",
        weight: -0.5,
        definition: "accept_with_fixes verdicts on the row's candidates.",
        group: "problem",
    },
    Term {
        key: "crew_rework",
        label: "Crew-caused rework",
        weight: -1.0,
        definition: "Repeat runs, [BLOCKED] or unmerged PRs, and operator requeues with cause crew, charged to the run that ended the attempt. Review verdicts are not counted again here.",
        group: "problem",
    },
    Term {
        key: "escaped_regressions",
        label: "Escaped regressions",
        weight: -2.0,
        definition: "Regressions with escape no_test against tasks whose final implementation run is the row's.",
        group: "problem",
    },
];

type Key = (String, String);

fn key(attempt: &Attempt) -> Key {
    row_key(
        attempt.runner.crew.as_deref(),
        attempt.runner.model.as_deref(),
    )
}

fn tier(task: &TaskFacts) -> &'static str {
    task.complexity.map_or("unassessed", |tier| tier.as_str())
}

/// One task per row, however many attempts that row started in the window.
fn rows(facts: &AgentFacts, since: DateTime<Utc>) -> Vec<RowInput<'_>> {
    let mut work: BTreeMap<Key, Vec<&TaskFacts>> = BTreeMap::new();
    for task in &facts.tasks {
        let keys: BTreeSet<_> = task
            .attempts
            .iter()
            .filter(|a| a.start >= since)
            .map(key)
            .collect();
        for row in keys {
            work.entry(row).or_default().push(task);
        }
    }
    let mut charges: BTreeMap<&str, Vec<&Charge>> = BTreeMap::new();
    let mut credits: BTreeMap<&str, Vec<&Credit>> = BTreeMap::new();
    for charge in &facts.charges {
        charges.entry(&charge.task_id).or_default().push(charge);
    }
    for credit in &facts.credits {
        credits.entry(&credit.task_id).or_default().push(credit);
    }
    let mut rows: Vec<_> = work
        .iter()
        .map(|(row, tasks)| row_input(facts, since, row.clone(), tasks, &charges, &credits))
        .collect();
    let mut fleet: BTreeMap<&str, Fleet> = BTreeMap::new();
    for tasks in work.values() {
        for task in tasks {
            fleet.entry(tier(task)).or_default().units += 1;
        }
    }
    for row in &rows {
        for (term, tally) in &row.tallies {
            for (task, count) in tally.task_counts() {
                *fleet
                    .entry(tier(task))
                    .or_default()
                    .counts
                    .entry(term)
                    .or_default() += count;
            }
        }
    }
    // Sparse tiers join the largest tier (lexical first on a tie). This
    // bounds their reference by a supported pool and conserves fleet totals.
    let largest = fleet
        .iter()
        .max_by(|(left, a), (right, b)| a.units.cmp(&b.units).then_with(|| right.cmp(left)))
        .map(|(tier, _)| *tier);
    if let Some(largest) = largest {
        let pool_key = |tier: &'static str| {
            if fleet[tier].units < MIN_TIER_TASKS {
                largest
            } else {
                tier
            }
        };
        let mut pools: BTreeMap<&str, Fleet> = BTreeMap::new();
        for (&tier, stats) in &fleet {
            let pool = pools.entry(pool_key(tier)).or_default();
            pool.units += stats.units;
            for (&term, &count) in &stats.counts {
                *pool.counts.entry(term).or_default() += count;
            }
        }
        for (row, tasks) in rows.iter_mut().zip(work.values()) {
            row.expected = Some(
                TERMS
                    .iter()
                    .map(|term| {
                        let expected = tasks
                            .iter()
                            .map(|task| {
                                let pool = &pools[pool_key(tier(task))];
                                pool.counts.get(term.key).copied().unwrap_or(0.0)
                                    / pool.units as f64
                            })
                            .sum();
                        (term.key, expected)
                    })
                    .collect(),
            );
        }
    }
    rows
}

/// A later accept cannot erase an earlier reject of this row's candidate.
fn first_review<'a>(task: &'a TaskFacts, row: &Key, since: DateTime<Utc>) -> Option<&'a Review> {
    task.reviews.iter().find(|review| {
        task.attempt_at(review.at).is_some_and(|index| {
            let attempt = &task.attempts[index];
            attempt.start >= since && key(attempt) == *row
        })
    })
}

/// Minimum fleet row-task observations before a tier has its own rate.
const MIN_TIER_TASKS: usize = 30;

#[derive(Default)]
struct Fleet {
    units: usize,
    counts: BTreeMap<&'static str, f64>,
}

fn row_input<'a>(
    facts: &AgentFacts,
    since: DateTime<Utc>,
    row: Key,
    tasks: &[&'a TaskFacts],
    charges: &BTreeMap<&str, Vec<&Charge>>,
    credits: &BTreeMap<&str, Vec<&Credit>>,
) -> RowInput<'a> {
    let mut tallies: BTreeMap<&'static str, Tally<'a>> = BTreeMap::new();
    let mut tiers: BTreeMap<&str, usize> = BTreeMap::new();
    let mut durations = Vec::new();
    let mut run_ids = BTreeSet::new();
    let mut missing_runs = false;
    let mut runs_without_model = 0;
    let mut landed = 0.0;
    for &task in tasks {
        *tiers.entry(tier(task)).or_default() += 1;
        for attempt in task
            .attempts
            .iter()
            .filter(|a| a.start >= since && key(a) == row)
        {
            runs_without_model += usize::from(attempt.runner.model.is_none());
            if let Some(end) = attempt.end {
                durations.push((end - attempt.start).num_milliseconds().max(0) as f64 / 1000.0);
            }
            match attempt.runner.run_id.as_deref() {
                Some(id) => {
                    run_ids.insert(id);
                }
                None => missing_runs = true,
            }
        }
        if let Some(review) = first_review(task, &row, since)
            && review.verdict == ReviewVerdict::Accept
        {
            tallies.entry("first_pass_accepts").or_default().add(
                task,
                1.0,
                Some(Occurrence {
                    at: review.at,
                    detail: "first review accepted the candidate".into(),
                    run_id: task
                        .attempt_at(review.at)
                        .and_then(|index| task.attempts[index].runner.run_id.clone()),
                }),
            );
        }
        if credits
            .get(task.id.as_str())
            .into_iter()
            .flatten()
            .any(|credit| {
                credit.anchor >= since
                    && row_key(credit.crew.as_deref(), credit.model.as_deref()) == row
            })
        {
            landed += 1.0;
            tallies
                .entry("weighted_landed")
                .or_default()
                .add(task, 1.0, None);
        }
        for charge in charges
            .get(task.id.as_str())
            .into_iter()
            .flatten()
            .filter(|charge| {
                charge.anchor >= since
                    && charge.owner == Owner::Role(AgentRole::Implementers)
                    && row_key(charge.crew.as_deref(), charge.model.as_deref()) == row
            })
        {
            let term = match charge.problem {
                Problem::ReviewReject => "review_rejects",
                Problem::ReviewFixes => "reviewer_repairs",
                Problem::Regression => "escaped_regressions",
                problem if problem.is_rework() => "crew_rework",
                _ => continue,
            };
            tallies.entry(term).or_default().add(
                task,
                1.0,
                Some(Occurrence {
                    at: charge.at,
                    detail: charge.detail.clone(),
                    run_id: charge.run_id.clone(),
                }),
            );
        }
    }
    let cost = if missing_runs || run_ids.is_empty() {
        None
    } else {
        run_ids.iter().try_fold(0.0, |total, id| {
            facts
                .run_costs
                .get(*id)
                .copied()
                .flatten()
                .map(|cost| total + cost)
        })
    };
    durations.sort_by(f64::total_cmp);
    let median = if durations.is_empty() {
        None
    } else {
        let middle = durations.len() / 2;
        Some((durations[middle] + durations[(durations.len() - 1) / 2]) / 2.0)
    };
    RowInput {
        crew: row.0,
        model: row.1,
        units: tasks.len(),
        runs_without_model,
        provisional: None,
        tallies,
        expected: None,
        stats: vec![
            stat(
                "cost_per_weighted_landed",
                "Cost per landed task (USD)",
                cost.filter(|_| landed > 0.0)
                    .map(|cost| json!(cost / landed))
                    .unwrap_or(Value::Null),
            ),
            stat(
                "median_run_seconds",
                "Median completed run (seconds)",
                median.map(number).unwrap_or(Value::Null),
            ),
            stat(
                "tasks_by_tier",
                "Tasks by tier",
                json!(
                    tiers
                        .iter()
                        .map(|(tier, counts)| format!("{tier}: {counts}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ),
        ],
    }
}

fn stat(key: &'static str, label: &'static str, value: Value) -> Stat {
    Stat { key, label, value }
}
