//! The one admission wave the drain pass and the readiness projection share.
//!
//! Both build an [`AdmissionWave`] from the same snapshot and call
//! [`AdmissionWave::select`]. A rule added here cannot land in only one of them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::fs::lock_mode::FootprintLockModes;
use orbit_types::task::Task;
use orbit_types::workflow::ResourceThrottle;

use crate::application::admission::auto_admission::{
    AdmissionHolders, AdmissionSelection, select_admissions,
};
use crate::application::admission::backlog_exclusion::BacklogSnapshot;
use crate::application::admission::cpu_light::{CPU_LIGHT_LEAVES, LightBudget, ResourceGate};
use crate::application::admission::gap_fit::GapFit;
use crate::application::admission::unknown_footprint::{
    FootprintGuard, FootprintWait, whole_tree_holder,
};
use crate::application::job::PublishedResume;
use crate::runtime::task::locks::UnlockedContextPaths;

/// Light budget, gate and free slots before a caller reserves any for repair.
///
/// The pass reads [`Self::free_slots`] to size `owner_repair_fallback`, then
/// passes that pull's `reserved_slots` into [`AdmissionWave::build`].
/// Readiness reserves nothing.
pub(super) struct UnreservedAdmissionSlots {
    light_budget: LightBudget,
    gate: ResourceGate,
    /// `gate.free_slots` before repair reservations.
    pub(super) free_slots: usize,
}

/// What both callers know before the wave is built.
pub(super) struct AdmissionWaveInput<'a> {
    pub(super) snapshot: &'a BacklogSnapshot,
    pub(super) claimed: &'a BTreeSet<String>,
    /// Tasks held out of this wave (a leaf of this drain failed at job load).
    pub(super) held: &'a BTreeSet<String>,
    pub(super) slots: UnreservedAdmissionSlots,
    /// Slots kept back from the wave. The pass passes the repair pull's
    /// reservation; readiness passes `0`.
    pub(super) reserved_slots: usize,
    pub(super) candidate_pool_limit: usize,
    pub(super) max_active_leaf_runs: u64,
    /// Leaves already occupying a slot.
    pub(super) occupied: usize,
    pub(super) workspace_root: &'a Path,
    pub(super) unlocked: &'a UnlockedContextPaths,
    pub(super) lock_modes: &'a FootprintLockModes,
}

/// One drain wave: the budget, the ordered candidates and the selection.
pub(super) struct AdmissionWave<'a> {
    pub(super) light_budget: LightBudget,
    pub(super) gate: ResourceGate,
    /// Slots the wave may fill, after [`AdmissionWaveInput::reserved_slots`].
    pub(super) free_slots: usize,
    /// Admissible leaves minus claimed minus held, in snapshot order.
    pub(super) pending: Vec<String>,
    pub(super) holders: AdmissionHolders,
    /// Gate-filtered prefix of at most `candidate_pool_limit` candidates.
    pub(super) examined: Vec<String>,
    /// Gate-filtered candidates before the pool prefix was cut.
    candidate_count: usize,
    task_lookup: &'a BTreeMap<String, Task>,
    workspace_root: &'a Path,
    unlocked: &'a UnlockedContextPaths,
    published_resumes: &'a BTreeMap<String, PublishedResume>,
    footprint_enabled: bool,
    leaves_in_flight: bool,
    whole_tree_holder: Option<String>,
    footprint_waits: &'a BTreeMap<String, FootprintWait>,
}

/// The light budget, gate and unreserved free-slot count.
///
/// This is the only `LightBudget` and `ResourceGate` construction under
/// `workspace_auto`.
pub(super) fn unreserved_admission_slots<'a>(
    claimed: impl IntoIterator<Item = &'a String>,
    task_lookup: &BTreeMap<String, Task>,
    throttle: Option<&ResourceThrottle>,
    unthrottled_slots: usize,
) -> UnreservedAdmissionSlots {
    // [ORB-14624] A CPU-only throttle leaves the reserved light slots open.
    let light_budget = LightBudget::new(CPU_LIGHT_LEAVES, claimed, task_lookup);
    let gate = ResourceGate::new(throttle, &light_budget);
    let free_slots = gate.free_slots(unthrottled_slots, &light_budget);
    UnreservedAdmissionSlots {
        light_budget,
        gate,
        free_slots,
    }
}

impl<'a> AdmissionWave<'a> {
    /// Build the wave in snapshot order: pending, then the gate filter, then
    /// the examined prefix.
    pub(super) fn build(input: AdmissionWaveInput<'a>) -> Self {
        let UnreservedAdmissionSlots {
            light_budget,
            gate,
            free_slots,
        } = input.slots;
        let free_slots = free_slots.saturating_sub(input.reserved_slots);
        // Priority/age order is the snapshot's, and everything below preserves
        // it: the slots are scarce, so they go to the front of the queue.
        let pending: Vec<String> = input
            .snapshot
            .admissible_leaves
            .iter()
            .filter(|task_id| !input.claimed.contains(*task_id) && !input.held.contains(*task_id))
            .cloned()
            .collect();
        // Under a CPU-only throttle the wave is chosen from CPU-light leaves
        // alone, in the same order, so heavier work ahead of them does not
        // hide them from the candidate pool. The gate filter runs before the
        // pool prefix.
        let (candidate_count, examined) = if gate == ResourceGate::Open {
            let examined = pending
                .iter()
                .take(input.candidate_pool_limit)
                .cloned()
                .collect();
            (pending.len(), examined)
        } else {
            let filtered = pending.iter().filter(|task_id| {
                input
                    .snapshot
                    .task_lookup
                    .get(*task_id)
                    .is_some_and(|task| gate.admits(task))
            });
            let mut filtered = filtered.cloned();
            let examined: Vec<String> =
                filtered.by_ref().take(input.candidate_pool_limit).collect();
            let candidate_count = examined.len() + filtered.count();
            (candidate_count, examined)
        };
        let holders = AdmissionHolders::new(
            &input.snapshot.lock_holders,
            input.claimed,
            &input.snapshot.task_lookup,
            &input.snapshot.remote_claims,
            input.workspace_root,
            input.unlocked,
            input.lock_modes,
        );
        // [ORB-15191] Work with no footprint waits for its pilot or goes
        // alone, unless this drain runs one leaf at a time.
        let whole_tree_holder = whole_tree_holder(input.claimed, &input.snapshot.task_lookup);
        Self {
            light_budget,
            gate,
            free_slots,
            pending,
            holders,
            examined,
            candidate_count,
            task_lookup: &input.snapshot.task_lookup,
            workspace_root: input.workspace_root,
            unlocked: input.unlocked,
            published_resumes: &input.snapshot.published_resumes,
            footprint_enabled: input.max_active_leaf_runs > 1,
            leaves_in_flight: input.occupied > 0,
            whole_tree_holder,
            footprint_waits: &input.snapshot.footprint_waits,
        }
    }

    /// Fill the free slots. The pass calls this more than once under gap fit;
    /// readiness calls it once with `None`.
    pub(super) fn select(&self, gap_fit: Option<&mut GapFit<'_>>) -> AdmissionSelection {
        // [ORB-11973] Select a mutually compatible set, walking past a blocked
        // candidate to the next compatible one rather than stopping at it.
        select_admissions(
            &self.examined,
            self.task_lookup,
            self.workspace_root,
            self.unlocked,
            &self.holders,
            self.published_resumes,
            self.free_slots,
            FootprintGuard {
                enabled: self.footprint_enabled,
                leaves_in_flight: self.leaves_in_flight,
                whole_tree_holder: self.whole_tree_holder.clone(),
                waits: self.footprint_waits,
            },
            gap_fit,
        )
    }

    /// The pool bound hid work: the wave ran out of examined candidates
    /// before it ran out of slots.
    pub(super) fn candidate_pool_truncated(&self, selected_len: usize) -> bool {
        self.candidate_count > self.examined.len() && selected_len < self.free_slots
    }
}
