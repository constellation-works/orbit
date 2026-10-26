//! Orchestrators: ranked by the tasks' `orchestrator` attribution and the
//! `orchestrator_model` recorded when each was created (`model unknown` for a
//! task without one), over the tasks each row owns that were created in the
//! window. Credit is one point per landed task, whatever its complexity:
//! complexity is assessed when the task is filed or piloted, so weighting by
//! it would let the ranked party set its own credit. Problems are only those
//! the one-owner rule gives orchestrators, spec defects and spec-caused
//! rework, on the same per-task scale, so a spec problem cancels a landing.
//! Rework and regressions with no recorded cause are nobody's; they are
//! shown beside the score as its confidence, never scored.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use orbit_types::task::TaskOrigin;
use serde_json::json;

use super::RoleSpec;
use crate::application::agents::attribution::{Charge, Owner};
use crate::application::agents::facts::{Problem, TaskFacts};
use crate::application::agents::records::Cause;
use crate::application::agents::score::{Occurrence, RowInput, Stat, Tally, Term, number};
use crate::application::agents::{AgentFacts, AgentRole, MODEL_UNKNOWN};

pub(super) const ROLE: RoleSpec = RoleSpec {
    unit: "owned tasks",
    terms: TERMS,
    term_units: &[],
    rows: Some(rows),
    header: None,
};

const TERMS: &[Term] = &[
    Term {
        key: "landed",
        label: "Landed",
        weight: 1.0,
        definition: "Landed tasks, one each whatever their complexity, so per 100 owned tasks the percent that landed.",
        group: "credit",
    },
    Term {
        key: "spec_defects",
        label: "Spec defects",
        weight: -1.0,
        definition: "Description or criteria edited after the first run started (once per task), agent blocks naming the criteria, final-recovery escalations citing them, regressions with escape spec_ambiguous, and confirmed task-pilot holds on validation-tool, ADR-conflict, surface or utility findings. Pilot holds count at most once per task and add no charge when their confirming event already counts as a spec edit, criteria block or criteria escalation; prerequisite-only holds and unconfirmed warnings add none.",
        group: "problem",
    },
    Term {
        key: "spec_rework",
        label: "Spec-caused rework",
        weight: -1.0,
        definition: "Repeat runs, [BLOCKED] or unmerged PRs, and operator requeues whose cause is spec.",
        group: "problem",
    },
];

/// One row per orchestrator and recorded orchestrator model that owns a task
/// created since `since`; a task without a recorded model counts under
/// `model unknown`.
fn rows(facts: &AgentFacts, since: DateTime<Utc>) -> Vec<RowInput<'_>> {
    let mut owned: BTreeMap<(&str, &str), Vec<&TaskFacts>> = BTreeMap::new();
    for task in &facts.tasks {
        if let Some(name) = task.orchestrator.as_deref()
            && task.created_at >= since
        {
            let model = task.orchestrator_model.as_deref().unwrap_or(MODEL_UNKNOWN);
            owned.entry((name, model)).or_default().push(task);
        }
    }
    let mut charges: HashMap<&str, Vec<&Charge>> = HashMap::new();
    for charge in &facts.charges {
        charges
            .entry(charge.task_id.as_str())
            .or_default()
            .push(charge);
    }
    owned
        .into_iter()
        .map(|((name, model), tasks)| row(name, model, &tasks, &charges, facts.computed_at))
        .collect()
}

fn row<'a>(
    name: &str,
    model: &str,
    tasks: &[&'a TaskFacts],
    charges: &HashMap<&str, Vec<&Charge>>,
    now: DateTime<Utc>,
) -> RowInput<'a> {
    let mut tallies: BTreeMap<&'static str, Tally<'a>> = BTreeMap::new();
    let mut rework: BTreeMap<Cause, i64> = Cause::ALL.iter().map(|cause| (*cause, 0)).collect();
    let (mut dropped_initiative, mut unknown_origin, mut unclassified_regressions) = (0, 0, 0);
    let mut active = Vec::new();
    for &task in tasks {
        let initiative = task.origin == Some(TaskOrigin::OrchestratorInitiative);
        if task.landed {
            tallies.entry("landed").or_default().add(task, 1.0, None);
        }
        if initiative && task.dropped {
            dropped_initiative += 1;
        }
        if task.origin.is_none() {
            unknown_origin += 1;
        }
        if let (Some(first), Some(last)) = (task.attempts.first(), task.attempts.last()) {
            active.push((first.start, last.end.unwrap_or(now)));
        }
        for charge in charges.get(task.id.as_str()).into_iter().flatten() {
            if charge.problem.is_rework()
                && let Some(cause) = charge.cause
            {
                *rework.entry(cause).or_default() += 1;
            }
            if charge.problem == Problem::Regression
                && charge.owner == Owner::NotScored("unclassified")
            {
                unclassified_regressions += 1;
            }
            if charge.owner != Owner::Role(AgentRole::Orchestrators) {
                continue;
            }
            let key = if charge.problem.is_rework() {
                "spec_rework"
            } else {
                "spec_defects"
            };
            tallies.entry(key).or_default().add(
                task,
                1.0,
                Some(Occurrence {
                    at: charge.at,
                    detail: format!("{}: {}", charge.problem.as_str(), charge.detail),
                    run_id: charge.run_id.clone(),
                }),
            );
        }
    }
    let unclassified = rework[&Cause::Other] + unclassified_regressions;
    let stats = vec![
        stat(
            "unclassified",
            "Unclassified rework and regressions (not scored)",
            json!(unclassified),
        ),
        stat(
            "dropped_initiative",
            "Dropped initiative",
            json!(dropped_initiative),
        ),
        stat("unknown_origin", "Unknown origin", json!(unknown_origin)),
        stat("active_hours", "Active hours", number(union_hours(active))),
        stat(
            "rework_spec",
            "Rework: spec (counted)",
            json!(rework[&Cause::Spec]),
        ),
        stat(
            "rework_crew",
            "Rework: crew (counted against implementers)",
            json!(rework[&Cause::Crew]),
        ),
        stat(
            "rework_infrastructure",
            "Rework: infrastructure (not scored)",
            json!(rework[&Cause::Infrastructure]),
        ),
        stat(
            "rework_no_cause",
            "Rework: no cause (not scored)",
            json!(rework[&Cause::Other]),
        ),
        stat(
            "regressions_unclassified",
            "Regressions: unclassified (not scored)",
            json!(unclassified_regressions),
        ),
    ];
    RowInput {
        denominators: BTreeMap::new(),
        crew: name.to_string(),
        model: model.to_string(),
        units: tasks.len(),
        runs_without_model: 0,
        provisional: None,
        tallies,
        expected: None,
        stats,
    }
}

fn stat(key: &'static str, label: &'static str, value: serde_json::Value) -> Stat {
    Stat { key, label, value }
}

/// Total length of the union of `intervals`, in hours: the span from each
/// owned task's first run start to its last run end, now while one is open.
fn union_hours(mut intervals: Vec<(DateTime<Utc>, DateTime<Utc>)>) -> f64 {
    intervals.retain(|(start, end)| end > start);
    intervals.sort();
    let mut total = chrono::Duration::zero();
    let mut current: Option<(DateTime<Utc>, DateTime<Utc>)> = None;
    for (start, end) in intervals {
        match current {
            Some((open, close)) if start <= close => current = Some((open, close.max(end))),
            Some((open, close)) => {
                total += close - open;
                current = Some((start, end));
            }
            None => current = Some((start, end)),
        }
    }
    if let Some((open, close)) = current {
        total += close - open;
    }
    total.num_seconds() as f64 / 3600.0
}
