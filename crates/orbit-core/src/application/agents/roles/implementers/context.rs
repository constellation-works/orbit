//! Expected change size: the size-adjusted expectation the scored
//! escaped-regression term compares with, and the display-only diff
//! economy. Expected size comes from the fleet's task cell, never an
//! individual row's own diff.

use std::collections::BTreeMap;

use serde_json::Value;

use super::performance::{ESCAPED_REGRESSIONS, Key, configured_type, tier};
use crate::application::agents::facts::TaskFacts;
use crate::application::agents::score::{RowInput, Stat, number};
use crate::application::agents::{AgentFacts, WindowBounds};

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some((values[values.len() / 2] + values[(values.len() - 1) / 2]) / 2.0)
}

/// Append diff economy after all rows' tallies exist, so the fleet and the
/// rows count the same row-task observations, and return each row's
/// expected escaped regressions, in row order. Medians count unique landed
/// tasks.
pub(super) fn append_diff_context(
    facts: &AgentFacts,
    window: WindowBounds,
    work: &BTreeMap<Key, Vec<&TaskFacts>>,
    rows: &mut [RowInput<'_>],
) -> Vec<f64> {
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
        .filter_map(|(key, mut values)| median(&mut values).map(|median| (key, median)))
        .collect();
    // [0, 2), [2, 4), [4, 8), ... lines, plus an explicit unknown bucket.
    // Every observation, including the row itself, shares its cell's median.
    // Sparse buckets keep their own same-tier rate; they never pool tiers.
    let bucket = |task: &TaskFacts| {
        (
            tier(task),
            medians
                .get(&cell(task))
                .map(|size| size.max(1.0).log2().floor() as u32),
        )
    };
    let mut fleet: BTreeMap<_, (usize, f64)> = BTreeMap::new();
    for (row, tasks) in rows.iter().zip(work.values()) {
        for task in tasks {
            fleet.entry(bucket(task)).or_default().0 += 1;
        }
        if let Some(tally) = row.tallies.get(ESCAPED_REGRESSIONS) {
            for (task, count) in tally.task_counts() {
                fleet.entry(bucket(task)).or_default().1 += count;
            }
        }
    }
    let mut expected_regressions = Vec::with_capacity(rows.len());
    for (row, tasks) in rows.iter_mut().zip(work.values()) {
        expected_regressions.push(
            tasks
                .iter()
                .map(|task| {
                    let (units, count) = fleet[&bucket(task)];
                    count / units as f64
                })
                .sum(),
        );
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
            value: median(&mut ratios).map(number).unwrap_or(Value::Null),
        });
    }
    expected_regressions
}
