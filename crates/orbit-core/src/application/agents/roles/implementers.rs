//! Implementers: the crew and model of the implementation runs that did the
//! work. Costs and tier comparisons are context only, never score terms.

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
        label: "Weighted landed",
        weight: 1.0,
        definition: "Landed tasks whose final implementation run is the row's, weighted by complexity: low 1, medium 2, hard 4, xhard 8, unassessed or unset 1.",
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
    let mut fleet: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for task in &facts.tasks {
        let keys: BTreeSet<_> = task
            .attempts
            .iter()
            .filter(|a| a.start >= since)
            .map(key)
            .collect();
        for row in keys {
            if let Some(review) = first_review(task, &row, since) {
                let counts = fleet.entry(tier(task)).or_default();
                counts.0 += 1;
                counts.1 += usize::from(needs_changes(review.verdict));
            }
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
    work.into_iter()
        .map(|(row, tasks)| row_input(facts, since, row, &tasks, &fleet, &charges, &credits))
        .collect()
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

fn needs_changes(verdict: ReviewVerdict) -> bool {
    matches!(
        verdict,
        ReviewVerdict::Reject | ReviewVerdict::AcceptWithFixes
    )
}

fn row_input<'a>(
    facts: &AgentFacts,
    since: DateTime<Utc>,
    row: Key,
    tasks: &[&'a TaskFacts],
    fleet: &BTreeMap<&str, (usize, usize)>,
    charges: &BTreeMap<&str, Vec<&Charge>>,
    credits: &BTreeMap<&str, Vec<&Credit>>,
) -> RowInput<'a> {
    let mut tallies: BTreeMap<&'static str, Tally<'a>> = BTreeMap::new();
    let mut tiers: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
    let mut durations = Vec::new();
    let mut run_ids = BTreeSet::new();
    let mut missing_runs = false;
    let mut runs_without_model = 0;
    let mut weighted = 0.0;
    for &task in tasks {
        let counts = tiers.entry(tier(task)).or_default();
        counts.0 += 1;
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
        if let Some(review) = first_review(task, &row, since) {
            counts.1 += 1;
            counts.2 += usize::from(needs_changes(review.verdict));
            if review.verdict == ReviewVerdict::Accept {
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
        }
        for credit in credits
            .get(task.id.as_str())
            .into_iter()
            .flatten()
            .filter(|credit| {
                credit.anchor >= since
                    && row_key(credit.crew.as_deref(), credit.model.as_deref()) == row
            })
        {
            weighted += credit.weight as f64;
            tallies
                .entry("weighted_landed")
                .or_default()
                .add(task, credit.weight as f64, None);
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
    let tier_rows: Vec<Value> = tiers
        .iter()
        .map(|(tier, (tasks, reviewed, changes))| {
            let (fleet_reviewed, fleet_changes) = fleet.get(tier).copied().unwrap_or_default();
            json!({
                "tier": tier, "tasks": tasks, "first_reviews": reviewed, "needing_changes": changes,
                "needing_changes_percent": percent(*changes, *reviewed),
                "fleet_needing_changes_percent": percent(fleet_changes, fleet_reviewed),
            })
        })
        .collect();
    RowInput {
        crew: row.0,
        model: row.1,
        units: tasks.len(),
        runs_without_model,
        provisional: None,
        tallies,
        stats: vec![
            stat(
                "cost_per_weighted_landed",
                "Cost per weighted landed task (USD)",
                cost.filter(|_| weighted > 0.0)
                    .map(|cost| json!(cost / weighted))
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
                        .map(|(tier, counts)| format!("{tier}: {}", counts.0))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ),
            stat("tier_comparison", "First reviews by tier", json!(tier_rows)),
        ],
    }
}

fn percent(changes: usize, reviewed: usize) -> Value {
    if reviewed == 0 {
        Value::Null
    } else {
        number(changes as f64 * 100.0 / reviewed as f64)
    }
}

fn stat(key: &'static str, label: &'static str, value: Value) -> Stat {
    Stat { key, label, value }
}
