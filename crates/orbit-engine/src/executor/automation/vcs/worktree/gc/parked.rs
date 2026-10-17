//! Whole-worktree reclaim for runs whose tasks are parked in `someday`
//! [ORB-16385].
//!
//! A failed run keeps its worktree while its task is unsettled, and a task
//! parked in `someday` may wait there indefinitely. Once every task the
//! worktree serves has been parked for `worktree.parked_reclaim_after_hours`,
//! measured from the status change rather than the run's end, the worktree
//! goes through the same gates as a settled one, plus these: no live or
//! undecidable worker, no claim still owed to its owner, and, for a clean
//! tree, run branch commits that survive deleting the branch.

use super::super::rescue::{WorktreeGcRecovery, committed_recovery};
use super::leftovers::worker_may_be_alive;
use super::paths::attributed_task_ids;
use crate::context::RuntimeHost;
use chrono::{DateTime, Utc};
use orbit_types::workflow::JobRun;
use std::path::Path;

/// When the parked task that licenses a removal entered `someday`.
pub(super) struct Parked {
    pub(super) since: DateTime<Utc>,
}

impl Parked {
    pub(super) fn detail(&self) -> String {
        format!("task parked in someday since {}", self.since.to_rfc3339())
    }
}

/// Why a parked run's worktree must stay whatever its tree holds: the action
/// and its detail. A claim that is not settled may still deliver the leaf's
/// candidate to its owner, and a worker may still write it.
pub(super) fn parked_hold<H: RuntimeHost + ?Sized>(
    host: &H,
    run: &JobRun,
) -> Option<(&'static str, Option<String>)> {
    if worker_may_be_alive(run) {
        return Some(("skipped:worker_alive", None));
    }
    host.live_claim_for_worktree_gc(&run.run_id)
        .map(|claim| ("skipped:claim_live", Some(claim)))
}

/// The branch tip a clean parked worktree was checked at, and where its
/// commits survive the run branch's deletion; the error keeps the worktree
/// and its branch.
pub(super) fn parked_commits_survive(
    path: &Path,
    run: &JobRun,
    branch: &str,
) -> Result<(String, Option<WorktreeGcRecovery>), String> {
    committed_recovery(path, branch, &attributed_task_ids(run), &run.run_id)
}
