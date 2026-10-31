//! Priority-aware surface reservations for lock-blocked work.
//!
//! Dispatch order (critical, corrective or expiring, priority, expiry, age)
//! only ranks the tasks that are eligible at a pass. A high-priority task that
//! needs several locks is excluded until all of them are free. Each time one
//! frees, a smaller, lower-ranked task overlapping just that lock takes it,
//! and the high-priority task never sees all of its locks free at once.
//!
//! A reservation closes that race. A critical or high-priority task held back
//! only by locks other tasks hold reserves its own surface for the pass: a
//! backlog task ranked behind it whose surface overlaps it is withheld as
//! [`BacklogTaskExclusionReason::SurfaceReserved`], naming the reserving task
//! as its blocker. Nothing is persisted. Each pass recomputes reservations from
//! the snapshot, so one lapses as soon as the reserving task is admitted or
//! leaves `backlog`.
//!
//! Reservations are bounded so one stuck task cannot freeze the queue: only
//! [`reserves_surface`] tasks reserve, at most [`MAX_SURFACE_RESERVATIONS`] per
//! pass in dispatch order, a withheld task never reserves in turn, and only
//! tasks the queue already ranks behind the reserving task are withheld.
//! Work that does not overlap a reserved surface admits as before.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use orbit_common::fs::lock_mode::FootprintLockModes;
use orbit_types::task::{Task, TaskPriority, automatic_dispatch_cmp_with_expiry};

use super::backlog_exclusion::{
    BacklogTaskConflict, BacklogTaskExclusion, BacklogTaskExclusionReason,
};
use crate::application::job::PublishedResume;
use crate::runtime::task::locks::{
    UnlockedContextPaths, lock_context_files_for_task, lock_holder_index, task_lock_overlaps,
};

/// How many lock-blocked tasks may reserve their surface in one pass. The
/// earliest in dispatch order reserve; the rest wait on their locks as before.
pub(super) const MAX_SURFACE_RESERVATIONS: usize = 2;

/// Whether a task blocked only by held locks may reserve its surface: critical
/// and high priority only, so ordinary work never holds the queue for itself.
pub(super) fn reserves_surface(task: &Task) -> bool {
    matches!(task.priority, TaskPriority::Critical | TaskPriority::High)
}

/// The detail a reserving task's own lock-conflict exclusion carries, so the
/// reservation is visible from both sides.
pub(super) fn reserving_detail() -> String {
    "Reserves its surface while it waits on these locks: overlapping backlog work ranked \
     behind it is withheld until it is admitted or leaves backlog."
        .to_string()
}

/// Withhold every task in `backlog` that a reservation covers, recording it in
/// `excluded`, and return the rest in their original order.
///
/// `reserving` is the lock-blocked tasks that reserve this pass, already
/// bounded by the caller. A task is withheld only by a reserving task that
/// sorts ahead of it in dispatch order, using the same frozen-batch expiry
/// snapshot as the backlog sort. A task in `published_resumes` is never
/// withheld.
#[allow(clippy::too_many_arguments)]
pub(super) fn withhold_reserved_surfaces<'a>(
    backlog: Vec<&'a Task>,
    reserving: &[&Task],
    published_resumes: &BTreeMap<String, PublishedResume>,
    expiring_batches: &BTreeMap<String, DateTime<Utc>>,
    workspace_root: &Path,
    unlocked: &UnlockedContextPaths,
    lock_modes: &FootprintLockModes,
    excluded: &mut Vec<BacklogTaskExclusion>,
) -> Vec<&'a Task> {
    if reserving.is_empty() {
        return backlog;
    }
    let mut reserved: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for task in reserving {
        for selector in lock_context_files_for_task(task, workspace_root, unlocked) {
            reserved.entry(selector).or_default().push(task.id.clone());
        }
    }
    let reserved_index = lock_holder_index(&reserved);
    let reserving_by_id: BTreeMap<&str, &Task> = reserving
        .iter()
        .map(|task| (task.id.as_str(), *task))
        .collect();
    withhold_reserved_by(
        backlog,
        |task| task,
        |task| published_resumes.contains_key(&task.id),
        expiring_batches,
        |task| {
            task_lock_overlaps(task, &reserved_index, workspace_root, unlocked, lock_modes)
                .into_iter()
                .filter_map(|overlap| {
                    let reserver = *reserving_by_id.get(overlap.locking_task_id.as_str())?;
                    Some((overlap, reserver))
                })
                .collect()
        },
        |task, conflicts: Vec<BacklogTaskConflict>| {
            let mut reserved_for = conflicts
                .iter()
                .map(|conflict| conflict.locking_task_id.as_str())
                .collect::<Vec<_>>();
            reserved_for.sort_unstable();
            reserved_for.dedup();
            excluded.push(BacklogTaskExclusion {
                id: task.id.clone(),
                reason: BacklogTaskExclusionReason::SurfaceReserved,
                detail: Some(format!(
                    "Reserved for {}, ranked ahead of this task and waiting only on context \
                     locks. This task is admitted once the reserving task is admitted or leaves \
                     backlog.",
                    reserved_for.join(", ")
                )),
                conflicts,
                crew: None,
            });
        },
    )
}

/// The reservation policy over any overlap lookup: the drain expands live
/// selectors, the backlog forecast reads its precomputed conflict rows
/// [ORB-18686].
///
/// `overlaps` lists a candidate's overlaps with the reserving tasks'
/// surfaces, each with the reserving task it names; only those whose
/// reserving task sorts ahead of the candidate withhold it, and `withheld`
/// receives each withheld candidate with them. `exempt` names candidates a
/// reservation never withholds (a published resume). The rest are returned
/// in their original order.
pub(super) fn withhold_reserved_by<'a, 'r, T: Copy, C>(
    backlog: Vec<T>,
    task_of: impl Fn(T) -> &'a Task,
    exempt: impl Fn(&Task) -> bool,
    expiring_batches: &BTreeMap<String, DateTime<Utc>>,
    mut overlaps: impl FnMut(T) -> Vec<(C, &'r Task)>,
    mut withheld: impl FnMut(T, Vec<C>),
) -> Vec<T> {
    let mut kept = Vec::with_capacity(backlog.len());
    for candidate in backlog {
        let task = task_of(candidate);
        // Resuming a published delivery takes no lock at admission, so it
        // cannot take a reserved surface either.
        if exempt(task) {
            kept.push(candidate);
            continue;
        }
        let conflicts: Vec<C> = overlaps(candidate)
            .into_iter()
            .filter(|(_, reserver)| {
                automatic_dispatch_cmp_with_expiry(
                    reserver,
                    expiring_batches.contains_key(&reserver.id),
                    task,
                    expiring_batches.contains_key(&task.id),
                )
                .is_lt()
            })
            .map(|(conflict, _)| conflict)
            .collect();
        if conflicts.is_empty() {
            kept.push(candidate);
        } else {
            withheld(candidate, conflicts);
        }
    }
    kept
}
