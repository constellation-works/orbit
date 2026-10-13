//! The task population a backlog snapshot reads: every open task, plus the
//! closed ones its rules still reach. Closed tasks accumulate without bound
//! while the open population stays small, so selecting by indexed status keeps
//! a snapshot's envelope reads proportional to the open work.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_store::TaskStoreBackend;
use orbit_types::task::{Task, TaskStatus, has_epic_tag, validate_orb_task_id};

use super::BacklogSnapshot;
use super::ordering::MAX_TASK_PARENT_CHAIN_DEPTH;
use crate::OrbitRuntime;
use crate::application::task::{TaskListFilter, metadata_task};

/// Every task not in a terminal status, as envelope metadata, plus the closed
/// tasks the snapshot's tree rules read:
///
/// - each loaded task's ancestors, which the group-conflict roll-up walks to
///   find a task's root, at any status;
/// - each descendant of a `backlog` epic root that declares no context, which
///   decides whether that root inherited its whole footprint from below
///   ([`orbit_types::task::inherited_only_epic_roots`]). A nested epic owns
///   its own subtree, so the walk stops at one.
///
/// Every other closed task is invisible to admission, exclusion and readiness.
pub(super) fn admission_population(
    store: &dyn TaskStoreBackend,
) -> Result<BTreeMap<String, Task>, OrbitError> {
    let open = TaskListFilter {
        statuses: Some(
            TaskStatus::ALL
                .into_iter()
                .filter(|status| !status.is_terminal())
                .collect(),
        ),
        ..TaskListFilter::default()
    };
    let mut lookup = into_lookup(store.task_candidates(&open, usize::MAX)?.items);

    let mut visited = BTreeSet::new();
    let mut frontier = lookup
        .values()
        .filter(|task| {
            task.status == TaskStatus::Backlog
                && has_epic_tag(&task.tags)
                && task.context_files.is_empty()
        })
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    for _ in 0..MAX_TASK_PARENT_CHAIN_DEPTH {
        let mut next = Vec::new();
        for parent_id in frontier {
            if !visited.insert(parent_id.clone()) {
                continue;
            }
            let children = TaskListFilter {
                parent_id: Some(parent_id),
                ..TaskListFilter::default()
            };
            for child in store.task_candidates(&children, usize::MAX)?.items {
                if !has_epic_tag(&child.tags) {
                    next.push(child.id.clone());
                }
                lookup
                    .entry(child.id.clone())
                    .or_insert_with(|| metadata_task(child));
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }

    include_ancestors(store, &mut lookup)?;
    Ok(lookup)
}

impl BacklogSnapshot {
    /// Add the tasks in `ids` that the snapshot's population left out, at any
    /// status: a live leaf can still carry a task that has since closed, and
    /// an explicit readiness request may name one. An id no task in this
    /// workspace has, malformed ones included, stays absent.
    pub(crate) fn include_tasks<'a>(
        &mut self,
        runtime: &OrbitRuntime,
        ids: impl IntoIterator<Item = &'a String>,
    ) -> Result<(), OrbitError> {
        let missing = ids
            .into_iter()
            .filter(|id| !self.task_lookup.contains_key(*id) && validate_orb_task_id(id).is_ok())
            .cloned()
            .collect::<BTreeSet<_>>();
        if missing.is_empty() || !runtime.coordination_task_reads_visible() {
            return Ok(());
        }
        let envelopes = runtime.stores().tasks().task_envelopes_for_ids(&missing)?;
        self.task_lookup.extend(into_lookup(envelopes));
        Ok(())
    }
}

/// Load the missing ancestors of every task in `lookup`, one level per read.
fn include_ancestors(
    store: &dyn TaskStoreBackend,
    lookup: &mut BTreeMap<String, Task>,
) -> Result<(), OrbitError> {
    let mut requested = BTreeSet::new();
    let mut loaded = lookup.keys().cloned().collect::<Vec<_>>();
    for _ in 0..MAX_TASK_PARENT_CHAIN_DEPTH {
        let missing = loaded
            .iter()
            .filter_map(|id| lookup.get(id)?.parent_id())
            .filter(|parent_id| !lookup.contains_key(*parent_id))
            .map(ToOwned::to_owned)
            .filter(|parent_id| requested.insert(parent_id.clone()))
            .collect::<BTreeSet<_>>();
        if missing.is_empty() {
            break;
        }
        let parents = into_lookup(store.task_envelopes_for_ids(&missing)?);
        loaded = parents.keys().cloned().collect();
        lookup.extend(parents);
    }
    Ok(())
}

fn into_lookup(
    envelopes: impl IntoIterator<Item = orbit_types::task::TaskEnvelopeV2>,
) -> BTreeMap<String, Task> {
    envelopes
        .into_iter()
        .map(|envelope| (envelope.id.clone(), metadata_task(envelope)))
        .collect()
}
