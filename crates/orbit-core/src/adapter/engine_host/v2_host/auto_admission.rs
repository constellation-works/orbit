//! Conflict-aware admission selection for one auto-drain wave [ORB-11973].
//!
//! The classifier and the read-only readiness diagnostic both have to answer
//! "which backlog tasks may take a free leaf slot right now". Answering it
//! twice is how they drifted: each took a blind `free_slots`-long prefix of the
//! priority/age order, so a run of mutually overlapping candidates could fill
//! every slot and then serialize on `reserve_locks` inside
//! `task_gate_pipeline` while independent work stayed queued behind them.
//!
//! This module is the single answer. It walks the existing order and keeps a
//! candidate only when its effective lock footprint is free of every footprint
//! already spoken for — by a real task lock, by a live leaf wrapper's claim,
//! and by the candidates selected earlier in this same wave. Skipping a
//! blocked candidate does not skip the ones behind it, so the wave reaches
//! independent work instead of stopping at the cluster.
//!
//! Footprints come from `lock_context_files_for_task` and are compared with
//! `workspace_relative_paths_overlap`, so canonical file/directory/symbol
//! normalization and an epic root's descendant coverage are the same semantics
//! the gate will enforce later. Nothing here reserves anything: the wave is a
//! prediction, and `reserve_locks` stays authoritative for the race.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::fs::path::workspace_relative_paths_overlap;
use orbit_types::task::{Task, TaskStatus};
use serde_json::{Value, json};

use crate::runtime::task::locks::lock_context_files_for_task;

/// Where a blocking footprint came from, so a deferral says whether the lock is
/// genuinely held or merely planned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ConflictProvenance {
    /// The blocker holds a real task lock: it is `in-progress` or `review`.
    HeldLock,
    /// A live leaf wrapper is already carrying the blocker, which is still
    /// `backlog` because the wrapper's child has not moved it yet. Nothing is
    /// reserved, but the wrapper is on its way to `reserve_locks`, so admitting
    /// an overlapping candidate would only queue two waiters on one footprint.
    LiveClaim,
    /// The blocker was selected earlier in this same wave. No lock exists yet;
    /// this is the conflict the drain would otherwise create for itself.
    SameWave,
}

impl ConflictProvenance {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            ConflictProvenance::HeldLock => "held_lock",
            ConflictProvenance::LiveClaim => "live_claim",
            ConflictProvenance::SameWave => "same_wave",
        }
    }
}

/// One overlapping selector pair that kept a candidate out of the wave.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct AdmissionConflict {
    /// The candidate's own selector.
    pub(super) requested_selector: String,
    /// The selector it overlaps. Not necessarily equal: a `dir:` scope and a
    /// `file:` beneath it overlap without matching.
    pub(super) blocking_selector: String,
    pub(super) blocking_task_id: String,
    pub(super) provenance: ConflictProvenance,
}

/// A candidate that was examined, had a free slot available, and still could
/// not take it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DeferredAdmission {
    pub(super) task_id: String,
    pub(super) conflicts: Vec<AdmissionConflict>,
}

impl DeferredAdmission {
    /// The distinct tasks an operator would have to wait on, in a stable order.
    pub(super) fn blocking_task_ids(&self) -> Vec<String> {
        self.conflicts
            .iter()
            .map(|conflict| conflict.blocking_task_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(super) fn to_json(&self) -> Value {
        json!({
            "task_id": self.task_id,
            "blocking_task_ids": self.blocking_task_ids(),
            "conflicts": self
                .conflicts
                .iter()
                .map(|conflict| json!({
                    "requested_selector": conflict.requested_selector,
                    "blocking_selector": conflict.blocking_selector,
                    "blocking_task_id": conflict.blocking_task_id,
                    "provenance": conflict.provenance.as_str(),
                }))
                .collect::<Vec<_>>(),
        })
    }
}

/// The outcome of one wave, partitioned so every candidate is accounted for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct AdmissionSelection {
    /// Mutually compatible tasks, in the candidate order they were offered in.
    pub(super) selected: Vec<String>,
    /// Examined against a free slot and blocked.
    pub(super) deferred: Vec<DeferredAdmission>,
    /// Never examined: the wave was already full when they came up. These are
    /// saturated, not conflicted, and saying so keeps the two diagnoses apart.
    pub(super) queued_behind_capacity: Vec<String>,
}

impl AdmissionSelection {
    pub(super) fn deferred_for(&self, task_id: &str) -> Option<&DeferredAdmission> {
        self.deferred
            .iter()
            .find(|deferred| deferred.task_id == task_id)
    }

    pub(super) fn deferred_json(&self) -> Vec<Value> {
        self.deferred
            .iter()
            .map(DeferredAdmission::to_json)
            .collect()
    }
}

/// Every lock footprint a wave must respect, keyed by selector.
///
/// Held locks and live claims are kept apart rather than merged because the
/// remedies differ: a held lock clears when its task leaves `in-progress`, a
/// live claim clears when a wrapper's child finishes.
#[derive(Clone, Debug, Default)]
pub(super) struct AdmissionHolders {
    held: BTreeMap<String, Vec<String>>,
    claimed: BTreeMap<String, Vec<String>>,
}

impl AdmissionHolders {
    /// Build the pre-wave footprints from the snapshot's lock holders and the
    /// tasks live leaf wrappers are carrying.
    ///
    /// A claimed task that already reached `in-progress` or `review` is left to
    /// the held set: it is the same footprint, and reporting it twice under two
    /// provenances would misstate why the candidate is waiting.
    pub(super) fn new(
        lock_holders: &BTreeMap<String, Vec<String>>,
        live_claims: &BTreeSet<String>,
        task_lookup: &BTreeMap<String, Task>,
        workspace_root: &Path,
    ) -> Self {
        let mut claimed: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for task_id in live_claims {
            let Some(task) = task_lookup.get(task_id) else {
                continue;
            };
            if matches!(task.status, TaskStatus::InProgress | TaskStatus::Review) {
                continue;
            }
            for selector in lock_context_files_for_task(task, task_lookup, workspace_root) {
                claimed.entry(selector).or_default().push(task.id.clone());
            }
        }
        for claiming_task_ids in claimed.values_mut() {
            claiming_task_ids.sort();
            claiming_task_ids.dedup();
        }

        Self {
            held: lock_holders.clone(),
            claimed,
        }
    }

    fn conflicts_for(&self, requested_selector: &str) -> Vec<AdmissionConflict> {
        let held = self
            .held
            .iter()
            .map(|entry| (entry, ConflictProvenance::HeldLock));
        let claimed = self
            .claimed
            .iter()
            .map(|entry| (entry, ConflictProvenance::LiveClaim));

        held.chain(claimed)
            .filter(|((blocking_selector, _), _)| {
                workspace_relative_paths_overlap(requested_selector, blocking_selector)
            })
            .flat_map(|((blocking_selector, blocking_task_ids), provenance)| {
                blocking_task_ids
                    .iter()
                    .map(move |blocking_task_id| AdmissionConflict {
                        requested_selector: requested_selector.to_string(),
                        blocking_selector: blocking_selector.clone(),
                        blocking_task_id: blocking_task_id.clone(),
                        provenance,
                    })
            })
            .collect()
    }
}

/// Fill the free slots with a pairwise non-conflicting set, preserving the
/// candidate order among compatible tasks.
///
/// `ordered_candidates` is the caller's priority/age order, already filtered for
/// dependencies, crew, epic membership, live claims, and grant scope. This
/// function adds exactly one rule: a candidate joins the wave only if nothing
/// it would touch is already spoken for.
pub(super) fn select_admissions(
    ordered_candidates: &[String],
    task_lookup: &BTreeMap<String, Task>,
    workspace_root: &Path,
    holders: &AdmissionHolders,
    free_slots: usize,
) -> AdmissionSelection {
    let mut selection = AdmissionSelection::default();
    // Selectors this wave has already promised, and the task each was promised
    // to. Grown as tasks are selected, which is what makes the wave internally
    // consistent rather than merely consistent with the stores.
    let mut wave: BTreeMap<String, String> = BTreeMap::new();

    for candidate_id in ordered_candidates {
        if selection.selected.len() >= free_slots {
            selection.queued_behind_capacity.push(candidate_id.clone());
            continue;
        }
        let Some(task) = task_lookup.get(candidate_id) else {
            // A candidate the snapshot no longer knows cannot have its
            // footprint expanded, so it cannot be shown to be compatible.
            selection.queued_behind_capacity.push(candidate_id.clone());
            continue;
        };

        let footprint = lock_context_files_for_task(task, task_lookup, workspace_root);
        let mut conflicts = Vec::new();
        for requested_selector in &footprint {
            conflicts.extend(holders.conflicts_for(requested_selector));
            conflicts.extend(wave_conflicts(requested_selector, &wave));
        }
        conflicts.sort();
        conflicts.dedup();

        if conflicts.is_empty() {
            for selector in footprint {
                wave.entry(selector).or_insert_with(|| candidate_id.clone());
            }
            selection.selected.push(candidate_id.clone());
        } else {
            selection.deferred.push(DeferredAdmission {
                task_id: candidate_id.clone(),
                conflicts,
            });
        }
    }

    selection
}

fn wave_conflicts(
    requested_selector: &str,
    wave: &BTreeMap<String, String>,
) -> Vec<AdmissionConflict> {
    wave.iter()
        .filter(|(blocking_selector, _)| {
            workspace_relative_paths_overlap(requested_selector, blocking_selector)
        })
        .map(|(blocking_selector, blocking_task_id)| AdmissionConflict {
            requested_selector: requested_selector.to_string(),
            blocking_selector: blocking_selector.clone(),
            blocking_task_id: blocking_task_id.clone(),
            provenance: ConflictProvenance::SameWave,
        })
        .collect()
}
