//! Display-only diff discipline and regression comparisons. Expected size
//! comes from the fleet's task cell, never an individual row's own diff.

use super::{
    AgentFacts, BTreeMap, DateTime, Key, RowInput, Stat, Tally, TaskFacts, Utc, configured_type,
    number, stat, tier,
};
use serde_json::Value;

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some((values[values.len() / 2] + values[(values.len() - 1) / 2]) / 2.0)
}

/// Append context after all rows' tallies exist, so the fleet and the rows
/// count the same row-task observations. Medians count unique landed tasks.
pub(super) fn append_diff_context(
    facts: &AgentFacts,
    since: DateTime<Utc>,
    work: &BTreeMap<Key, Vec<&TaskFacts>>,
    rows: &mut [RowInput<'_>],
) {
    let cell = |task: &TaskFacts| {
        (
            tier(task),
            configured_type(task, since, &facts.performance_types),
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
        if let Some(tally) = row.tallies.get("escaped_regressions") {
            for (task, count) in tally.task_counts() {
                fleet.entry(bucket(task)).or_default().1 += count;
            }
        }
    }
    for (row, tasks) in rows.iter_mut().zip(work.values()) {
        let actual = row
            .tallies
            .get("escaped_regressions")
            .map_or(0.0, Tally::count);
        let expected: f64 = tasks
            .iter()
            .map(|task| {
                let (units, count) = fleet[&bucket(task)];
                count / units as f64
            })
            .sum();
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
        row.stats.extend([
            stat(
                "escaped_regressions",
                "Escaped regressions (severity-weighted)",
                number(actual),
            ),
            stat(
                "regression_residual",
                "Regression residual (actual − expected)",
                number(actual - expected),
            ),
            Stat {
                key: "diff_economy",
                label: "Diff economy (× fleet median)",
                value: median(&mut ratios).map(number).unwrap_or(Value::Null),
            },
        ]);
    }
}
