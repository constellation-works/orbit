use std::borrow::Borrow;
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_types::task::Task;

use crate::application::admission::census::DispatchOrder;

pub(super) const MAX_TASK_PARENT_CHAIN_DEPTH: usize = 32;

/// Sort owned or borrowed tasks into automatic dispatch order: critical
/// first, then corrective or expiring work, then priority, expiry, age, and
/// the task ID as the total tie-breaker, with `order`'s census affinity
/// between them [ORB-17304]. Pull admission sorts with the same comparator,
/// the owner as its requester here.
pub(crate) fn sort_tasks_for_automatic_dispatch<T: Borrow<Task>>(
    tasks: &mut [T],
    expiring: &BTreeMap<String, DateTime<Utc>>,
    order: &DispatchOrder,
) {
    order.sort(tasks, expiring);
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
