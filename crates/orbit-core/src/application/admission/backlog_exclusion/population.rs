//! The task population a backlog snapshot reads: every open task, plus the
//! closed ones its rules still reach. Closed tasks accumulate without bound
//! while the open population stays small, so selecting by indexed status keeps
//! a snapshot's envelope reads proportional to the open work.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_store::TaskV2Store;
use orbit_types::task::{Task, TaskStatus, has_epic_tag, validate_orb_task_id};

use super::BacklogSnapshot;
use super::ordering::MAX_TASK_PARENT_CHAIN_DEPTH;
use crate::OrbitRuntime;
use crate::application::task::{TaskListFilter, metadata_task};

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
        self.include_tasks_in(runtime, ids, false)
    }

    /// [`Self::include_tasks`] for an observational snapshot: the envelopes
    /// are read from the settled snapshot, with no commit recovery, so a
    /// readiness read never waits on a held task writer. A task whose bundle
    /// a writer holds in flight stays absent.
    pub(crate) fn include_tasks_observed<'a>(
        &mut self,
        runtime: &OrbitRuntime,
        ids: impl IntoIterator<Item = &'a String>,
    ) -> Result<(), OrbitError> {
        self.include_tasks_in(runtime, ids, true)
    }

    fn include_tasks_in<'a>(
        &mut self,
        runtime: &OrbitRuntime,
        ids: impl IntoIterator<Item = &'a String>,
        observed: bool,
    ) -> Result<(), OrbitError> {
        let missing = ids
            .into_iter()
            .filter(|id| !self.task_lookup.contains_key(*id) && validate_orb_task_id(id).is_ok())
            .cloned()
            .collect::<BTreeSet<_>>();
        if missing.is_empty() || !runtime.stored().task().reads_visible() {
            return Ok(());
        }
        let envelopes = envelopes_for_ids(runtime.stores().tasks(), &missing, observed)?;
        self.task_lookup.extend(into_lookup(envelopes));
        Ok(())
    }
}

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
///
/// `observed` selects the informational store read for the envelope
/// selections: no commit-boundary section, recovery, index repair or
/// persisted freshness proofs. Readiness — a read-only projection that
/// admits nothing — observes; the drain's own snapshot keeps the
/// coordinated read its admission decisions require. The ancestor loads
/// (`include_ancestors`) read envelopes by id in the same mode: only the
/// coordinated one settles a pending commit first.
pub(super) fn admission_population(
    store: &TaskV2Store,
    observed: bool,
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
    let mut lookup = into_lookup(candidates(store, &open, observed)?.items);

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
            for child in candidates(store, &children, observed)?.items {
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

    include_ancestors(store, &mut lookup, observed)?;
    Ok(lookup)
}

/// Load the missing ancestors of every task in `lookup`, one level per read.
fn include_ancestors(
    store: &TaskV2Store,
    lookup: &mut BTreeMap<String, Task>,
    observed: bool,
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
        let parents = into_lookup(envelopes_for_ids(store, &missing, observed)?);
        loaded = parents.keys().cloned().collect();
        lookup.extend(parents);
    }
    Ok(())
}

/// The unbounded envelope selection of one population phase, through the
/// coordinated or the observational store read (see
/// [`admission_population`]).
fn candidates(
    store: &TaskV2Store,
    filter: &TaskListFilter,
    observed: bool,
) -> Result<orbit_store::contract::TaskCandidates, OrbitError> {
    if observed {
        store.observe_task_candidates(filter, usize::MAX)
    } else {
        store.task_candidates(filter, usize::MAX)
    }
}

/// Envelopes of `ids` by keyed read, through the coordinated or the
/// observational store read (see [`admission_population`]).
fn envelopes_for_ids(
    store: &TaskV2Store,
    ids: &BTreeSet<String>,
    observed: bool,
) -> Result<Vec<orbit_types::task::TaskEnvelopeV2>, OrbitError> {
    if observed {
        store.observe_task_envelopes_for_ids(ids)
    } else {
        store.task_envelopes_for_ids(ids)
    }
}

fn into_lookup(
    envelopes: impl IntoIterator<Item = orbit_types::task::TaskEnvelopeV2>,
) -> BTreeMap<String, Task> {
    envelopes
        .into_iter()
        .map(|envelope| (envelope.id.clone(), metadata_task(envelope)))
        .collect()
}
