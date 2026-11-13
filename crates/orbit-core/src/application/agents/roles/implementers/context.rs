//! Expected change size: the size-adjusted expectation the scored
//! escaped-regression term compares with, and the display-only diff
//! economy. Expected size comes from the fleet's task cell, never an
//! individual row's own diff.

use std::collections::BTreeMap;

use serde_json::Value;

use super::performance::{ESCAPED_REGRESSIONS, Key, configured_type, tier};
use crate::application::agents::facts::TaskFacts;
use crate::application::agents::score::{self, RowInput, Stat, number};
use crate::application::agents::{AgentFacts, WindowBounds};

/// Append diff economy after all rows' tallies exist, so the fleet and the
/// rows count the same row-task observations, and return each row's
/// expected escaped regressions per task, in row and then task order (the
/// order of `work`). A row's expectation is the sum of its tasks'. Medians
/// count unique landed tasks.
pub(super) fn append_diff_context(
    facts: &AgentFacts,
    window: WindowBounds,
    work: &BTreeMap<Key, Vec<&TaskFacts>>,
    rows: &mut [RowInput<'_>],
) -> Vec<Vec<f64>> {
    let cell = |task: &TaskFacts| {
        (
            tier(task),
            configured_type(task, window, &facts.performance_types),
        )
    };
    let mut landed = BTreeMap::new();
    for row in rows.iter() {
        if let Some(tally) = row.tallies.get("weighted_landed") {
            for (task, _) in tally.task_counts() {
                landed.insert(task.id.as_str(), task);
            }
        }
    }
    let mut sizes: BTreeMap<_, Vec<f64>> = BTreeMap::new();
    for task in landed.values() {
        if let Some(&lines) = facts.landed_lines.get(&task.id) {
            sizes.entry(cell(task)).or_default().push(lines as f64);
        }
    }
    let medians: BTreeMap<_, _> = sizes
        .into_iter()
        .filter_map(|(key, mut values)| score::median(&mut values).map(|median| (key, median)))
        .collect();
    // Size medians include the fleet; regression rates exclude the entire row.
    let bucket = |task: &TaskFacts| {
        (
            tier(task),
            medians
                .get(&cell(task))
                .map(|size| size.max(1.0).log2().floor() as u32),
        )
    };
    let mut fleet: BTreeMap<_, (usize, f64)> = BTreeMap::new();
    let mut by_row = Vec::new();
    for (row, tasks) in rows.iter().zip(work.values()) {
        let mut own: BTreeMap<_, (usize, f64)> = BTreeMap::new();
        for task in tasks {
            own.entry(bucket(task)).or_default().0 += 1;
        }
        if let Some(tally) = row.tallies.get(ESCAPED_REGRESSIONS) {
            for (task, count) in tally.task_counts() {
                own.entry(bucket(task)).or_default().1 += count;
            }
        }
        for (&key, &(units, count)) in &own {
            let total = fleet.entry(key).or_default();
            total.0 += units;
            total.1 += count;
        }
        by_row.push(own);
    }
    let mut expected_regressions = Vec::with_capacity(rows.len());
    for ((row, tasks), own) in rows.iter_mut().zip(work.values()).zip(&by_row) {
        let mut fallback = false;
        let expected: Vec<f64> = tasks
            .iter()
            .map(|task| {
                let key = bucket(task);
                let peers = |key: &(_, _), total: &(usize, f64)| {
                    let own = own.get(key).copied().unwrap_or_default();
                    (total.0 - own.0, total.1 - own.1)
                };
                let (mut units, mut count) = peers(&key, &fleet[&key]);
                if units == 0 {
                    fallback = true;
                    (units, count) = fleet
                        .iter()
                        .filter(|(other, _)| other.0 == key.0)
                        .map(|(key, total)| peers(key, total))
                        .fold((0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1));
                    if units == 0 {
                        (units, count) = fleet
                            .iter()
                            .map(|(key, total)| peers(key, total))
                            .fold((0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1));
                    }
                }
                if units == 0 {
                    0.0
                } else {
                    count / units as f64
                }
            })
            .collect();
        expected_regressions.push(expected);
        row.escaped_regressions_fallback = Some(fallback);
        let mut ratios = Vec::new();
        if let Some(tally) = row.tallies.get("weighted_landed") {
            for (task, _) in tally.task_counts() {
                if let Some(&lines) = facts.landed_lines.get(&task.id)
                    && let Some(&reference) = medians.get(&cell(task))
                    && reference > 0.0
                {
                    ratios.push(lines as f64 / reference);
                }
            }
        }
        row.stats.push(Stat {
            key: "diff_economy",
            label: "Diff economy (× fleet median)",
            value: score::median(&mut ratios)
                .map(number)
                .unwrap_or(Value::Null),
        });
    }
    expected_regressions
}
