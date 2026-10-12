use std::borrow::Borrow;
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_types::task::Task;

pub(super) const MAX_TASK_PARENT_CHAIN_DEPTH: usize = 32;

/// Sort owned or borrowed tasks into automatic dispatch order: critical
/// first, then corrective or expiring work, then priority, expiry, age, and
/// the task ID as the total tie-breaker. Uses the same expiry-aware comparison
/// as surface reservations (`automatic_dispatch_cmp_with_expiry`).
pub(in crate::adapter::engine_host::v2_host) fn sort_tasks_for_automatic_dispatch<
    T: Borrow<Task>,
>(
    tasks: &mut [T],
    expiring: &BTreeMap<String, DateTime<Utc>>,
) {
    tasks.sort_by(|left, right| {
        let (left, right) = (left.borrow(), right.borrow());
        orbit_types::task::automatic_dispatch_cmp_with_expiry(
            left,
            expiring.contains_key(&left.id),
            right,
            expiring.contains_key(&right.id),
        )
    });
}

pub(super) fn task_root_id(task: &Task, task_lookup: &BTreeMap<String, Task>) -> String {
    let mut path = vec![task.id.clone()];
    let mut root_id = task.id.clone();
    let mut next_parent_id = task.parent_id().map(ToOwned::to_owned);

    for _ in 0..MAX_TASK_PARENT_CHAIN_DEPTH {
        let Some(parent_id) = next_parent_id else {
            return root_id;
        };

        if let Some(cycle_start) = path.iter().position(|task_id| task_id == &parent_id) {
            return path[cycle_start..].iter().min().cloned().unwrap_or(root_id);
        }

        let Some(parent) = task_lookup.get(&parent_id) else {
            return root_id;
        };

        root_id = parent.id.clone();
        path.push(parent.id.clone());
        next_parent_id = parent.parent_id().map(ToOwned::to_owned);
    }

    root_id
}
