//! Admission of backlog work that declares no footprint [ORB-15191].
//!
//! A task with no `context_files` and no `no-diff-expected` tag holds no file
//! lock, so a multi-slot drain used to admit it beside anything — including
//! concurrent tasks editing the same files. Its footprint is unknown, so it is
//! treated as the whole tree:
//!
//! - While a task pilot on this host would still prepare it, it waits for the
//!   pilot to persist selectors (`awaiting_footprint`). Once it has them it is
//!   ordinary work under the ordinary lock rules.
//! - When no pilot will — none is enabled for it, the pilot assessed it and
//!   left it without selectors, or the bounded wait elapsed — it is admitted
//!   only alone: with no other leaf in flight and nothing else selected in
//!   its wave (`awaiting_exclusive_slot`). The first such waiter reserves the
//!   tree, so lower-ranked editing work stops filling slots until it starts.
//! - While it runs, it holds the tree: no other editing work is admitted.
//!
//! Work tagged `no-diff-expected` edits nothing, so it neither waits nor is
//! held. A single-slot run cannot race itself and an explicitly selected
//! ship names its own work, so neither applies the rule.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_engine::DispatchError;
use orbit_types::task::{Task, declares_no_footprint};

use crate::OrbitRuntime;
use crate::application::automation::preparation::FootprintPilots;

use super::auto_admission::{
    AdmissionConflict, ConflictProvenance, DeferralReason, DeferredAdmission,
};

/// The stand-in selector a whole-tree conflict names on both sides: the
/// blocker declared no footprint, so every path is spoken for.
pub(in crate::adapter::engine_host::v2_host) const WHOLE_TREE_SELECTOR: &str = "*";

/// Why a candidate with no footprint is not admitted like ordinary work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::adapter::engine_host::v2_host) enum FootprintWait {
    /// A task pilot on this host will prepare it; it waits until `until` at
    /// the latest.
    AwaitingFootprint { until: DateTime<Utc> },
    /// No pilot will prepare it; it runs only alone.
    Exclusive,
}

impl FootprintWait {
    /// The operator-facing explanation and fix.
    pub(in crate::adapter::engine_host::v2_host) fn detail(&self) -> String {
        match self {
            Self::AwaitingFootprint { until } => format!(
                "This task declares no context_files, so it would hold no file lock and could \
                 conflict with concurrent work. A multi-slot drain waits for the task pilot to \
                 prepare its footprint, until {}; after that it runs only alone. Set \
                 context_files, or tag no-diff-expected if no diff is expected, to admit it now.",
                until.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            ),
            Self::Exclusive => "This task declares no context_files and no task pilot will \
                 prepare them, so its footprint is the whole tree: a multi-slot drain starts it \
                 only when no other leaf is in flight, and admits no other editing work while \
                 it runs. Set context_files, or tag no-diff-expected if no diff is expected."
                .to_string(),
        }
    }
}

/// Classify every task among `tasks` that declares no footprint.
///
/// Pilots are read once, and only when such a task exists; a task's comments
/// only when a pilot would still prepare it.
pub(in crate::adapter::engine_host::v2_host) fn footprint_waits<'a>(
    runtime: &OrbitRuntime,
    action: &str,
    tasks: impl IntoIterator<Item = &'a Task>,
    now: DateTime<Utc>,
) -> Result<BTreeMap<String, FootprintWait>, DispatchError> {
    let mut pilots: Option<FootprintPilots> = None;
    let mut waits = BTreeMap::new();
    for task in tasks {
        if !declares_no_footprint(&task.tags, &task.context_files) {
            continue;
        }
        let pilots = pilots.get_or_insert_with(|| FootprintPilots::load(runtime));
        let wait = match pilots.prepares_until(task).filter(|until| *until > now) {
            Some(until) => {
                let assessed = runtime.task_pilot_assessed(&task.id).map_err(|error| {
                    DispatchError::DeterministicActionFailed {
                        action: action.to_string(),
                        message: format!("read task-pilot assessment of {}: {error}", task.id),
                    }
                })?;
                if assessed {
                    FootprintWait::Exclusive
                } else {
                    FootprintWait::AwaitingFootprint { until }
                }
            }
            None => FootprintWait::Exclusive,
        };
        waits.insert(task.id.clone(), wait);
    }
    Ok(waits)
}

/// What one wave knows about unknown footprints before it selects anything.
pub(in crate::adapter::engine_host::v2_host) struct FootprintGuard<'a> {
    /// Off for a single-slot run, which applies none of this.
    pub(in crate::adapter::engine_host::v2_host) enabled: bool,
    /// Whether any leaf already occupies a slot.
    pub(in crate::adapter::engine_host::v2_host) leaves_in_flight: bool,
    /// A live leaf carrying work with no footprint; it holds the tree.
    pub(in crate::adapter::engine_host::v2_host) whole_tree_holder: Option<String>,
    /// The candidates with no footprint, from [`footprint_waits`].
    pub(in crate::adapter::engine_host::v2_host) waits: &'a BTreeMap<String, FootprintWait>,
}

/// The first live-leaf task, in ID order, that declares no footprint.
pub(in crate::adapter::engine_host::v2_host) fn whole_tree_holder(
    live_task_ids: &BTreeSet<String>,
    task_lookup: &BTreeMap<String, Task>,
) -> Option<String> {
    live_task_ids
        .iter()
        .filter_map(|task_id| task_lookup.get(task_id))
        .find(|task| declares_no_footprint(&task.tags, &task.context_files))
        .map(|task| task.id.clone())
}

/// The guard applied candidate by candidate across one wave, in dispatch
/// order. [`Self::awaiting`] runs before the capacity check (a pilot wait is
/// worth reporting even when no slot is free); [`Self::check`] after it;
/// [`Self::selected`] once a candidate takes a slot.
pub(in crate::adapter::engine_host::v2_host) struct FootprintWave<'a> {
    guard: FootprintGuard<'a>,
    /// Whatever holds the tree now, and how.
    whole_tree: Option<(String, ConflictProvenance)>,
    /// A leaf is in flight or an editing candidate was selected this wave.
    busy: bool,
}

impl<'a> FootprintWave<'a> {
    pub(in crate::adapter::engine_host::v2_host) fn new(guard: FootprintGuard<'a>) -> Self {
        let whole_tree = guard
            .whole_tree_holder
            .clone()
            .filter(|_| guard.enabled)
            .map(|holder| (holder, ConflictProvenance::UnknownFootprint));
        let busy = guard.leaves_in_flight;
        Self {
            guard,
            whole_tree,
            busy,
        }
    }

    fn wait(&self, task: &Task) -> Option<&FootprintWait> {
        self.guard
            .enabled
            .then(|| self.guard.waits.get(&task.id))
            .flatten()
    }

    /// The deferral of a candidate still waiting for its pilot.
    pub(in crate::adapter::engine_host::v2_host) fn awaiting(
        &self,
        task: &Task,
    ) -> Option<DeferredAdmission> {
        let wait @ FootprintWait::AwaitingFootprint { .. } = self.wait(task)? else {
            return None;
        };
        Some(DeferredAdmission {
            task_id: task.id.clone(),
            reason: DeferralReason::AwaitingFootprint,
            conflicts: Vec::new(),
            detail: Some(wait.detail()),
        })
    }

    /// The deferral of a candidate a footprint rule keeps out of a free slot,
    /// or `None` to judge it by the ordinary lock rules. `edits` is whether
    /// its own footprint is non-empty.
    pub(in crate::adapter::engine_host::v2_host) fn check(
        &mut self,
        task: &Task,
        edits: bool,
    ) -> Option<DeferredAdmission> {
        if !self.guard.enabled {
            return None;
        }
        let wait = self.guard.waits.get(&task.id);
        if (edits || wait.is_some())
            && let Some((holder, provenance)) = &self.whole_tree
        {
            return Some(DeferredAdmission {
                task_id: task.id.clone(),
                reason: DeferralReason::Conflict,
                conflicts: vec![AdmissionConflict {
                    requested_selector: WHOLE_TREE_SELECTOR.to_string(),
                    blocking_selector: WHOLE_TREE_SELECTOR.to_string(),
                    blocking_task_id: holder.clone(),
                    provenance: *provenance,
                }],
                detail: None,
            });
        }
        if wait == Some(&FootprintWait::Exclusive) && self.busy {
            // The first waiter reserves the tree, or a busy drain would never
            // run it.
            self.whole_tree = Some((task.id.clone(), ConflictProvenance::ExclusiveReservation));
            return Some(DeferredAdmission {
                task_id: task.id.clone(),
                reason: DeferralReason::AwaitingExclusiveSlot,
                conflicts: Vec::new(),
                detail: Some(FootprintWait::Exclusive.detail()),
            });
        }
        None
    }

    /// Record that `task` took a slot.
    pub(in crate::adapter::engine_host::v2_host) fn selected(&mut self, task: &Task, edits: bool) {
        if !self.guard.enabled {
            return;
        }
        if self.guard.waits.contains_key(&task.id) {
            self.whole_tree = Some((task.id.clone(), ConflictProvenance::SameWave));
            self.busy = true;
        } else if edits {
            self.busy = true;
        }
    }
}
