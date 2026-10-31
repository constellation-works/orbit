use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_config::{CrewPerformanceType, classify_task_type};
use orbit_types::workflow::ReviewVerdict;
use serde_json::{Value, json};

use crate::application::agents::attribution::{Charge, Credit, Owner, row_key};
use crate::application::agents::facts::{Attempt, Problem, Review, TaskFacts};
use crate::application::agents::score::{Occurrence, RowInput, Stat, Tally, Term, number};
use crate::application::agents::{AgentFacts, AgentRole};

pub(super) const TERMS: &[Term] = &[
    Term {
        key: "weighted_landed",
        label: "Landed",
        weight: 1.0,
        definition: "Landed tasks whose final implementation run is the row's, one point each, whatever their complexity. Tasks tagged no-diff-expected when an attempt was admitted are not implementer work and are left out of every implementer count.",
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
        key: "mocks_matched",
        label: "Mocks matched",
        weight: 0.25,
        definition: "verify-mock-ui mocks the comparison review (purpose visual_review) showed on the row's first candidate with no finding its in-line verifier CONFIRMED at medium or above, 0.25 each: how well the implementer's self-check worked.",
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
];

pub(super) type Key = (String, String);

pub(super) fn key(attempt: &Attempt) -> Key {
    row_key(
        attempt.runner.crew.as_deref(),
        attempt.runner.model.as_deref(),
    )
}

pub(super) fn tier(task: &TaskFacts) -> &'static str {
    task.complexity.map_or("unassessed", |tier| tier.as_str())
}

/// Whether an attempt counts as implementation in the window: it started
/// in the window and the task was not `no-diff-expected` work when the
/// attempt was admitted. The one filter the headline rows, the fleet
/// expectations and the by-type cells all share.
pub(super) fn counted(attempt: &Attempt, since: DateTime<Utc>) -> bool {
    attempt.start >= since && !attempt.no_diff()
}

/// Each row's counted tasks: one task per row, however many counted
/// attempts that row started in the window.
pub(super) fn work_by_row(
    facts: &AgentFacts,
    since: DateTime<Utc>,
) -> BTreeMap<Key, Vec<&TaskFacts>> {
    let mut work: BTreeMap<Key, Vec<&TaskFacts>> = BTreeMap::new();
    for task in &facts.tasks {
        let keys: BTreeSet<_> = task
            .attempts
            .iter()
            .filter(|a| counted(a, since))
            .map(key)
            .collect();
        for row in keys {
            work.entry(row).or_default().push(task);
        }
    }
    work
}

pub(super) fn rows(
    facts: &AgentFacts,
    since: DateTime<Utc>,
    _min_support: usize,
) -> Vec<RowInput<'_>> {
    let work = work_by_row(facts, since);
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
    super::context::append_diff_context(facts, since, &work, &mut rows);
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
            counted(attempt, since) && key(attempt) == *row
        })
    })
}

/// Minimum fleet row-task observations before a tier has its own rate.
pub(super) const MIN_TIER_TASKS: usize = 30;

#[derive(Default)]
pub(super) struct Fleet {
    pub(super) units: usize,
    pub(super) counts: BTreeMap<&'static str, f64>,
}

/// The scored term tallies of one row (or one cell's task subset).
pub(super) fn compute_tallies<'a>(
    row: &Key,
    tasks: &[&'a TaskFacts],
    charges: &BTreeMap<&str, Vec<&Charge>>,
    credits: &BTreeMap<&str, Vec<&Credit>>,
    since: DateTime<Utc>,
) -> BTreeMap<&'static str, Tally<'a>> {
    let mut tallies: BTreeMap<&'static str, Tally<'a>> = BTreeMap::new();
    for &task in tasks {
        if let Some(review) = first_review(task, row, since)
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
        if let Some(visual) = &task.visual_review
            && visual.mocks_matched > 0
            && task.attempt_at(visual.at).is_some_and(|index| {
                let attempt = &task.attempts[index];
                counted(attempt, since) && key(attempt) == *row
            })
        {
            tallies.entry("mocks_matched").or_default().add(
                task,
                visual.mocks_matched as f64,
                Some(Occurrence {
                    at: visual.at,
                    detail: format!(
                        "{} mock(s) matched on the first candidate",
                        visual.mocks_matched
                    ),
                    run_id: None,
                }),
            );
        }
        if credits
            .get(task.id.as_str())
            .into_iter()
            .flatten()
            .any(|credit| {
                credit.anchor >= since
                    && row_key(credit.crew.as_deref(), credit.model.as_deref()) == *row
            })
        {
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
                    && row_key(charge.crew.as_deref(), charge.model.as_deref()) == *row
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
                charge.severity_weight(),
                Some(Occurrence {
                    at: charge.at,
                    detail: charge.detail.clone(),
                    run_id: charge.run_id.clone(),
                }),
            );
        }
    }
    tallies
}

fn row_input<'a>(
    facts: &AgentFacts,
    since: DateTime<Utc>,
    row: Key,
    tasks: &[&'a TaskFacts],
    charges: &BTreeMap<&str, Vec<&Charge>>,
    credits: &BTreeMap<&str, Vec<&Credit>>,
) -> RowInput<'a> {
    let tallies = compute_tallies(&row, tasks, charges, credits, since);
    // Reviewed tasks of this row: the denominator review-derived rates
    // need to tell a measured zero from no review at all. Display only;
    // it never enters `denominators`, so scores are unchanged.
    let reviewed_units = tasks
        .iter()
        .filter(|task| first_review(task, &row, since).is_some())
        .count();
    let landed = tallies.get("weighted_landed").map_or(0.0, Tally::count);
    let mut tiers: BTreeMap<&str, usize> = BTreeMap::new();
    let mut durations = Vec::new();
    let mut run_ids = BTreeSet::new();
    let mut missing_runs = false;
    let mut runs_without_model = 0;
    // Cost coverage over the same counted attempts the cost calculation
    // walks, attempt by attempt rather than by distinct run: a run shared
    // by two attempts covers both, and one attempt without a priced run
    // suppresses the complete-cohort average below. Display only; it
    // never enters tallies, denominators or scores.
    let mut cost_attempts = 0usize;
    let mut cost_measured_attempts = 0usize;
    let mut cost_unpriced_attempts = 0usize;
    let mut cost_missing_run_attempts = 0usize;
    for &task in tasks {
        *tiers.entry(tier(task)).or_default() += 1;
        for attempt in task
            .attempts
            .iter()
            .filter(|a| counted(a, since) && key(a) == row)
        {
            runs_without_model += usize::from(attempt.runner.model.is_none());
            if let Some(end) = attempt.end {
                durations.push((end - attempt.start).num_milliseconds().max(0) as f64 / 1000.0);
            }
            cost_attempts += 1;
            match attempt.runner.run_id.as_deref() {
                Some(id) => {
                    run_ids.insert(id);
                    if facts.run_costs.get(id).copied().flatten().is_some() {
                        cost_measured_attempts += 1;
                    } else {
                        cost_unpriced_attempts += 1;
                    }
                }
                None => {
                    missing_runs = true;
                    cost_missing_run_attempts += 1;
                }
            }
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
        denominators: BTreeMap::new(),
        reviewed_units: Some(reviewed_units),
        unjudged: false,
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
                "cost_coverage",
                "Cost coverage (counted attempts)",
                json!(format!(
                    "{cost_measured_attempts} measured · {cost_unpriced_attempts} unpriced · {cost_missing_run_attempts} missing run of {cost_attempts} counted attempts"
                )),
            ),
            stat(
                "cost_coverage_attempts",
                "Cost coverage denominator (counted attempts)",
                json!(cost_attempts),
            ),
            stat(
                "cost_measured_attempts",
                "Cost measured attempts",
                json!(cost_measured_attempts),
            ),
            stat(
                "cost_unpriced_attempts",
                "Cost unpriced attempts",
                json!(cost_unpriced_attempts),
            ),
            stat(
                "cost_missing_run_attempts",
                "Cost missing-run attempts",
                json!(cost_missing_run_attempts),
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

pub(super) fn stat(key: &'static str, label: &'static str, value: Value) -> Stat {
    Stat { key, label, value }
}

/// The tags in force when a task's counted work was admitted (its earliest
/// counted attempt in the window), so a tag added after an attempt started
/// cannot reclassify work already scored.
pub(super) fn admitted_tags(task: &TaskFacts, since: DateTime<Utc>) -> &[String] {
    task.attempts
        .iter()
        .filter(|a| counted(a, since))
        .min_by_key(|a| a.start)
        .map_or(&task.tags, |a| &a.tags)
}

/// The currently configured type of a task, classified as the by-type
/// cells classify it.
pub(super) fn configured_type(
    task: &TaskFacts,
    since: DateTime<Utc>,
    types: &[CrewPerformanceType],
) -> String {
    classify_task_type(types, admitted_tags(task, since), &task.context_files).to_string()
}

/// The path a context-file selector names, without its prefix. `module:`
/// and `command:` selectors name no path.
pub(super) fn selector_path(selector: &str) -> Option<&str> {
    if let Some(path) = selector.strip_prefix("file:") {
        Some(path)
    } else if let Some(path) = selector.strip_prefix("dir:") {
        Some(path)
    } else if let Some(remainder) = selector.strip_prefix("symbol:") {
        let (path, _) = remainder.split_once('#')?;
        if path.is_empty() { None } else { Some(path) }
    } else if selector.starts_with("module:") || selector.starts_with("command:") {
        None
    } else {
        Some(selector)
    }
}

/// The top-level path prefixes of a task's context-file selectors: the
/// first path component of each named path, deduplicated and sorted.
pub(super) fn path_prefixes(task: &TaskFacts) -> Vec<String> {
    let mut prefixes = BTreeSet::new();
    for selector in &task.context_files {
        if let Some(path) = selector_path(selector)
            && let Some(first) = path.split('/').find(|part| !part.is_empty())
        {
            prefixes.insert(first.to_string());
        }
    }
    prefixes.into_iter().collect()
}
