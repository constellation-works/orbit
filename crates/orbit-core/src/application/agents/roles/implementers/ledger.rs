//! The model ranking's view of the implementer scorecard: one record per
//! implemented task of each crew-and-model row, carrying the scorecard's own
//! attributed counts and same-mix expectations for that task, so the ranking
//! pools, resamples and compares them without recomputing either.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};

use super::performance::{TERMS, counted, evaluate_implementers, key, tier};
use crate::application::agents::facts::ExecutionMode;
use crate::application::agents::{AgentFacts, ExecutionScope, WindowBounds};

/// One implemented task of one crew-and-model row.
pub(in crate::application::agents) struct LedgerTask {
    /// The pilot's tier, or `unassessed`.
    pub(in crate::application::agents) tier: &'static str,
    /// The row's earliest counted attempt start on the task.
    pub(in crate::application::agents) start: DateTime<Utc>,
    /// Whether a review of the row's candidate settled.
    pub(in crate::application::agents) reviewed: bool,
    /// The scorecard's attributed count per term on this task.
    pub(in crate::application::agents) actual: BTreeMap<&'static str, f64>,
    /// The scorecard's expected count per term for this task.
    pub(in crate::application::agents) expected: BTreeMap<&'static str, f64>,
    /// Recorded cost of the row's runs on the task; `None` when the task is
    /// not managed or any counted attempt has no priced run.
    pub(in crate::application::agents) cost: Option<f64>,
}

/// One crew-and-model row of the implementer scorecard and its tasks.
pub(in crate::application::agents) struct LedgerRow {
    pub(in crate::application::agents) crew: String,
    pub(in crate::application::agents) model: String,
    pub(in crate::application::agents) tasks: Vec<LedgerTask>,
}

/// Every implementer row of the window with its tasks, in scorecard order.
pub(in crate::application::agents) fn implementer_ledger(
    facts: &AgentFacts,
    window: WindowBounds,
    execution: ExecutionScope,
) -> Vec<LedgerRow> {
    let evaluated = evaluate_implementers(facts, window, execution);
    evaluated
        .rows
        .iter()
        .zip(&evaluated.tasks)
        .map(|(row, tasks)| {
            let row_key = (row.crew.clone(), row.model.clone());
            let tasks = tasks
                .iter()
                .map(|expectation| {
                    let task = expectation.task;
                    let actual = TERMS
                        .iter()
                        .map(|term| {
                            let count = row
                                .tallies
                                .get(term.key)
                                .into_iter()
                                .flat_map(|tally| tally.task_counts())
                                .filter(|(counted_task, _)| counted_task.id == task.id)
                                .map(|(_, count)| count)
                                .sum();
                            (term.key, count)
                        })
                        .collect();
                    let attempts: Vec<_> = task
                        .attempts
                        .iter()
                        .filter(|attempt| counted(attempt, window) && key(attempt) == row_key)
                        .collect();
                    let start = attempts
                        .iter()
                        .map(|attempt| attempt.start)
                        .min()
                        .unwrap_or(task.created_at);
                    let cost = if task.execution == ExecutionMode::Managed {
                        let runs: Option<BTreeSet<&str>> = attempts
                            .iter()
                            .map(|attempt| attempt.runner.run_id.as_deref())
                            .collect();
                        runs.filter(|runs| !runs.is_empty()).and_then(|runs| {
                            runs.into_iter().try_fold(0.0, |total, id| {
                                facts
                                    .run_costs
                                    .get(id)
                                    .copied()
                                    .flatten()
                                    .map(|cost| total + cost)
                            })
                        })
                    } else {
                        None
                    };
                    LedgerTask {
                        tier: tier(task),
                        start,
                        reviewed: expectation.reviewed,
                        actual,
                        expected: expectation.expected.clone(),
                        cost,
                    }
                })
                .collect();
            LedgerRow {
                crew: row.crew.clone(),
                model: row.model.clone(),
                tasks,
            }
        })
        .collect()
}
