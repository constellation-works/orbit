//! The host-wide perf lane as an admission resource.
//!
//! `scripts/perf/paired.py` and `build-pair.sh` hold one host-wide lock for
//! their whole run, so two tasks that measure with the paired protocol cannot
//! both finish their evidence at once. A leaf that loses the lane stops with a
//! blocker after the implementation is already written. Admission therefore
//! treats the lane as a resource: while one perf task runs, no second one is
//! admitted, and it waits as `perf_lane_busy` naming the task that holds it.
//!
//! A task needs the lane when it carries `performance` or `perf-review`: the
//! tags the `perf-review` auto-task files for a hard timing criterion. The
//! admission snapshot carries no task descriptions, so tags are the only signal.

use std::collections::{BTreeMap, BTreeSet};

use orbit_types::task::{Task, TaskStatus};

/// Whether `task` is a perf task that needs the host-wide perf lane.
pub(crate) fn needs_perf_lane(task: &Task) -> bool {
    orbit_types::task::needs_perf_lane(&task.tags)
}

/// The first perf task, in ID order, already running: `in-progress`, or still
/// `backlog` but carried by a live leaf wrapper.
pub(crate) fn perf_lane_holder(
    live_claims: &BTreeSet<String>,
    task_lookup: &BTreeMap<String, Task>,
) -> Option<String> {
    task_lookup
        .values()
        .filter(|task| needs_perf_lane(task))
        .find(|task| task.status == TaskStatus::InProgress || live_claims.contains(&task.id))
        .map(|task| task.id.clone())
}

/// What an operator reads for a task waiting on the lane.
pub(crate) fn perf_lane_detail(holder: &str) -> String {
    orbit_types::task::perf_lane_detail(holder)
}
