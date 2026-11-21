//! The host-wide perf lane as an admission resource.
//!
//! `scripts/perf/paired.py` and `build-pair.sh` hold one host-wide lock for
//! their whole run, so two tasks that measure with the paired protocol cannot
//! both finish their evidence at once. A leaf that loses the lane stops with a
//! blocker after the implementation is already written. Admission therefore
//! treats the lane as a resource: while one perf task runs, no second one is
//! admitted, and it waits as `perf_lane_busy` naming the task that holds it.
//!
//! A task needs the lane when it carries [`PERF_LANE_TAGS`]: the tags the
//! `perf-review` auto-task files for a hard timing criterion. The admission
//! snapshot carries no task descriptions, so tags are the only signal.

use std::collections::{BTreeMap, BTreeSet};

use orbit_types::task::{Task, TaskStatus};

/// Tags that mark a task whose acceptance runs the paired timing protocol.
pub(crate) const PERF_LANE_TAGS: [&str; 2] = ["performance", "perf-review"];

/// Whether `task` is a perf task that needs the host-wide perf lane.
pub(crate) fn needs_perf_lane(task: &Task) -> bool {
    task.tags
        .iter()
        .any(|tag| PERF_LANE_TAGS.contains(&tag.as_str()))
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
    format!(
        "Waits for the host-wide perf lane, held by {holder}. Tasks that run the paired timing \
         protocol (tagged `performance` or `perf-review`) are admitted one at a time, so this \
         one starts when {holder} leaves `in-progress`."
    )
}
