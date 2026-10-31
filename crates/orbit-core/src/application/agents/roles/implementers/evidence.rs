use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_config::CrewPerformanceType;
use serde_json::{Value, json};

use super::performance::{
    Fleet, MIN_TIER_TASKS, TERMS, admitted_tags, compute_tallies, configured_type, path_prefixes,
    tier, work_by_row,
};
use crate::application::agents::AgentFacts;
use crate::application::agents::attribution::{Charge, Credit};
use crate::application::agents::facts::TaskFacts;
use crate::application::agents::score::{Tally, number};

/// One row per implemented task and the row (crew, model) that worked it:
/// the task's configured type, its tags as admitted, its context-file
/// selectors and top-level path prefixes, and its per-term counts under
/// that row. Summed per `(crew, model, tier, type)`, the counts reproduce
/// the by-type cells' `n` and, through [`raw_against`] with the same fleet
/// references, their `raw` values. Read-only: it only slices the scan the
/// scorecard already made.
pub(in crate::application::agents::roles) fn per_task_rows(
    facts: &AgentFacts,
    since: DateTime<Utc>,
    types: &[CrewPerformanceType],
) -> Vec<Value> {
    let work = work_by_row(facts, since);
    let mut charges: BTreeMap<&str, Vec<&Charge>> = BTreeMap::new();
    let mut credits: BTreeMap<&str, Vec<&Credit>> = BTreeMap::new();
    for charge in &facts.charges {
        charges.entry(&charge.task_id).or_default().push(charge);
    }
    for credit in &facts.credits {
        credits.entry(&credit.task_id).or_default().push(credit);
    }
    let mut rows = Vec::new();
    for (row, tasks) in &work {
        for &task in tasks {
            let tallies = compute_tallies(row, &[task], &charges, &credits, since);
            let counts: serde_json::Map<String, Value> = TERMS
                .iter()
                .map(|term| {
                    (
                        term.key.to_string(),
                        number(tallies.get(term.key).map_or(0.0, Tally::count)),
                    )
                })
                .collect();
            rows.push(json!({
                "task_id": task.id,
                "crew": row.0,
                "model": row.1,
                "tier": tier(task),
                "type": configured_type(task, since, types),
                "tags": admitted_tags(task, since),
                "context_files": task.context_files,
                "path_prefixes": path_prefixes(task),
                "counts": counts,
            }));
        }
    }
    rows.sort_by(|left, right| {
        left["crew"]
            .as_str()
            .cmp(&right["crew"].as_str())
            .then(left["model"].as_str().cmp(&right["model"].as_str()))
            .then(left["tier"].as_str().cmp(&right["tier"].as_str()))
            .then(left["type"].as_str().cmp(&right["type"].as_str()))
            .then(left["task_id"].as_str().cmp(&right["task_id"].as_str()))
    });
    rows
}

/// Shrinkage strength of the by-type matrix, at both levels.
const CELL_SHRINKAGE: f64 = 30.0;

/// The raw score of one task subset against `fleet` rates: 100 plus the
/// weighted actual-minus-expected rates. `None` for an empty subset.
fn raw_against(
    tallies: &BTreeMap<&'static str, Tally<'_>>,
    tasks: &[&TaskFacts],
    fleet: &Fleet,
) -> Option<f64> {
    if tasks.is_empty() || fleet.units == 0 {
        return None;
    }
    let n = tasks.len() as f64;
    let mut raw = 100.0;
    for term in TERMS {
        let actual = tallies.get(term.key).map_or(0.0, Tally::count);
        let rate = fleet.counts.get(term.key).copied().unwrap_or(0.0) / fleet.units as f64;
        raw += term.weight * (actual - rate * n) * 100.0 / n;
    }
    Some(raw)
}

/// One cell of the `(crew, model, tier, type)` matrix per row, tier and type
/// (configured types in order, then `other`), including empty cells. Raw
/// cell scores take their fleet expectation from the same `(tier, type)`
/// fleet cell, falling back to the tier's rates under 30 fleet observations;
/// the two-sided two-level shrinkage of the crew-performance scope then
/// pulls each cell toward its crew's tier mean and that mean toward 100.
pub(in crate::application::agents::roles) fn by_type_cells(
    facts: &AgentFacts,
    since: DateTime<Utc>,
    types: &[CrewPerformanceType],
) -> Vec<Value> {
    let work = work_by_row(facts, since);
    let mut charges: BTreeMap<&str, Vec<&Charge>> = BTreeMap::new();
    let mut credits: BTreeMap<&str, Vec<&Credit>> = BTreeMap::new();
    for charge in &facts.charges {
        charges.entry(&charge.task_id).or_default().push(charge);
    }
    for credit in &facts.credits {
        credits.entry(&credit.task_id).or_default().push(credit);
    }
    // Classify by the tags in force when the counted work was admitted
    // (the earliest counted attempt in the window), so a tag added after
    // an attempt started cannot retype work already scored.
    let task_type = |task: &TaskFacts| configured_type(task, since, types);
    let mut fleet_tier: BTreeMap<&str, Fleet> = BTreeMap::new();
    let mut fleet_cell: BTreeMap<(&str, String), Fleet> = BTreeMap::new();
    for tasks in work.values() {
        for task in tasks {
            fleet_tier.entry(tier(task)).or_default().units += 1;
            fleet_cell
                .entry((tier(task), task_type(task)))
                .or_default()
                .units += 1;
        }
    }
    for (row, tasks) in &work {
        let tallies = compute_tallies(row, tasks, &charges, &credits, since);
        for (&term, tally) in &tallies {
            for (task, count) in tally.task_counts() {
                *fleet_tier
                    .entry(tier(task))
                    .or_default()
                    .counts
                    .entry(term)
                    .or_default() += count;
                *fleet_cell
                    .entry((tier(task), task_type(task)))
                    .or_default()
                    .counts
                    .entry(term)
                    .or_default() += count;
            }
        }
    }
    let mut type_names: Vec<String> = types.iter().map(|entry| entry.name.clone()).collect();
    type_names.push(orbit_config::OTHER_TYPE.to_string());
    let mut cells = Vec::new();
    for (row, tasks) in &work {
        // The crew's evidence and raw score per tier, for the crew mean.
        let mut tier_tasks: BTreeMap<&str, Vec<&TaskFacts>> = BTreeMap::new();
        let mut cell_tasks: BTreeMap<(&str, String), Vec<&TaskFacts>> = BTreeMap::new();
        for task in tasks {
            tier_tasks.entry(tier(task)).or_default().push(task);
            cell_tasks
                .entry((tier(task), task_type(task)))
                .or_default()
                .push(task);
        }
        for tier_name in fleet_tier.keys() {
            let tier_subset: Vec<&TaskFacts> =
                tier_tasks.get(tier_name).cloned().unwrap_or_default();
            let n_tier = tier_subset.len();
            let tier_tallies = compute_tallies(row, &tier_subset, &charges, &credits, since);
            let raw_tier = raw_against(&tier_tallies, &tier_subset, &fleet_tier[tier_name]);
            let crew_mean = raw_tier.map_or(100.0, |raw| {
                (n_tier as f64 * raw + CELL_SHRINKAGE * 100.0) / (n_tier as f64 + CELL_SHRINKAGE)
            });
            for type_name in &type_names {
                let subset: Vec<&TaskFacts> = cell_tasks
                    .get(&(*tier_name, type_name.clone()))
                    .cloned()
                    .unwrap_or_default();
                let n = subset.len();
                let empty_fleet = Fleet::default();
                let cell_fleet = fleet_cell
                    .get(&(*tier_name, type_name.clone()))
                    .unwrap_or(&empty_fleet);
                let reference = if cell_fleet.units >= MIN_TIER_TASKS {
                    cell_fleet
                } else {
                    &fleet_tier[tier_name]
                };
                let cell_tallies = compute_tallies(row, &subset, &charges, &credits, since);
                let raw = raw_against(&cell_tallies, &subset, reference).unwrap_or(crew_mean);
                let score =
                    (n as f64 * raw + CELL_SHRINKAGE * crew_mean) / (n as f64 + CELL_SHRINKAGE);
                cells.push(json!({
                    "crew": row.0,
                    "model": row.1,
                    "tier": tier_name,
                    "type": type_name,
                    "n": n,
                    "raw": number(raw),
                    "crew_mean": number(crew_mean),
                    "score": number(score),
                }));
            }
        }
    }
    cells
}
