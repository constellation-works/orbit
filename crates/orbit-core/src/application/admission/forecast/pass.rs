use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

use chrono::Duration;
use resource_fit::{PassFit, ResourceFit};

use crate::application::admission::surface_reservation::{
    MAX_SURFACE_RESERVATIONS, withhold_reserved_by,
};
use crate::application::admission::unknown_footprint::{
    FootprintGuard, FootprintWait, FootprintWave, whole_tree_holder,
};
use crate::runtime::host_resource::profile::ProfileSource;

use super::model::{ForecastAdmission, ForecastPassScratch, ForecastWait};
use super::task_set::ForecastTaskSet;
use super::{ForecastHostEvidence, RESOURCE_RECHECK_HOURS, resource_fit};

impl ForecastAdmission {
    pub(crate) fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Every task index in the owner's dispatch order.
    pub(crate) fn dispatch_order(&self) -> &[usize] {
        &self.order
    }

    /// Hours from the forecast until which a task pilot may still prepare
    /// task `index`'s footprint.
    pub(crate) fn footprint_deadline(&self, index: usize) -> Option<f64> {
        self.footprint_deadline[index]
    }

    /// Earliest simulated hour for this task's schedule. Operator overrides
    /// are resolved when the admission snapshot is frozen.
    pub(crate) fn scheduled_deadline(&self, index: usize) -> Option<f64> {
        self.tasks[index].not_before.map(|at| {
            (at - self.forecast_at)
                .to_std()
                .map_or(0.0, |span| span.as_secs_f64() / 3600.0)
        })
    }

    /// A stable digest of task `index`'s canonical lock surface.
    pub(crate) fn surface_hash(&self, index: usize) -> &str {
        &self.surface_hash[index]
    }

    /// Whether task `index`'s surface overlaps a lock a `review` task holds.
    /// The forecast does not yet model review residuals, so it counts these.
    pub(crate) fn overlaps_review_lock(&self, index: usize) -> bool {
        self.review_lock_overlaps[index]
    }

    /// Judge candidates by gap fit against `evidence`, as an enforcing drain
    /// on that host does.
    pub(crate) fn fit_resources(&mut self, evidence: ForecastHostEvidence) {
        self.resource_fit = Some(ResourceFit::new(evidence, &self.tasks));
    }

    /// The profile gap fit prices task `index` by; `None` without resource
    /// fit.
    pub(crate) fn profile_source(&self, index: usize) -> Option<ProfileSource> {
        self.resource_fit
            .as_ref()
            .map(|fit| fit.profile_source(index))
    }

    /// When a pass that gap fit held should be retried: after
    /// [`RESOURCE_RECHECK_HOURS`], while a running delivery's profile still
    /// changes; `None` once only a completion can make room.
    pub(crate) fn resource_recheck(
        &self,
        running: &ForecastTaskSet,
        admitted_at: &[f64],
        at_hours: f64,
    ) -> Option<f64> {
        self.resource_fit
            .as_ref()
            .filter(|fit| fit.curve_changes(running, admitted_at, at_hours))
            .map(|_| at_hours + RESOURCE_RECHECK_HOURS)
    }

    pub(crate) fn scratch(&self) -> ForecastPassScratch {
        ForecastPassScratch {
            direct: vec![false; self.len()],
            blocked_roots: vec![false; self.roots],
            wave: ForecastTaskSet::with_len(self.len()),
            ..Default::default()
        }
    }

    /// One admission pass at `at_hours` after the forecast. `ready` is the
    /// pending tasks whose dependencies are satisfied, in dispatch order;
    /// `running` the tasks holding a slot. Pushes the admitted tasks, in
    /// dispatch order, to `selected`, and reports every other ready task's
    /// first failing gate to `wait`. `slots` is the drain's ceiling: a
    /// single-slot drain applies no unknown-footprint rule. `admitted_at`
    /// holds each running task's admission hour, which gap fit ages its
    /// profile by.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn pass(
        &self,
        ready: &[usize],
        running: &ForecastTaskSet,
        admitted_at: &[f64],
        free_slots: usize,
        slots: usize,
        at_hours: f64,
        scratch: &mut ForecastPassScratch,
        selected: &mut Vec<usize>,
        mut wait: impl FnMut(usize, ForecastWait),
    ) {
        // Held locks, as `backlog_snapshot` applies them: a direct conflict
        // excludes its whole root group, and the first lock-blocked
        // critical/high tasks reserve their surface.
        scratch.blocked_roots.fill(false);
        for &candidate in ready {
            let direct = self.held[candidate].intersects(running);
            scratch.direct[candidate] = direct;
            if direct {
                scratch.blocked_roots[self.root[candidate]] = true;
            }
        }
        scratch.kept.clear();
        scratch.reserving.clear();
        for &candidate in ready {
            if !scratch.blocked_roots[self.root[candidate]] {
                scratch.kept.push(candidate);
                continue;
            }
            if scratch.direct[candidate]
                && scratch.reserving.len() < MAX_SURFACE_RESERVATIONS
                && self.reserves[candidate]
            {
                scratch.reserving.push(candidate);
            }
            wait(candidate, ForecastWait::Lock);
        }
        let mut kept = std::mem::take(&mut scratch.kept);
        if !scratch.reserving.is_empty() {
            let reserving = &scratch.reserving;
            kept = withhold_reserved_by(
                kept,
                |index| &self.tasks[index],
                |_| false,
                &self.expiring,
                |index| {
                    reserving
                        .iter()
                        .filter(|reserver| self.surface[index].contains(**reserver))
                        .map(|reserver| ((), &self.tasks[*reserver]))
                        .collect()
                },
                |index, _| wait(index, ForecastWait::Reservation),
            );
        }

        // The wave, as `select_admissions` fills it.
        let waits = self.footprint_waits_at(&kept, at_hours);
        let waits = waits.as_ref().unwrap_or(&self.exclusive_waits);
        let holder = self.whole_tree_holder(running);
        let mut footprint = FootprintWave::new(FootprintGuard {
            enabled: slots > 1,
            leaves_in_flight: running.words.iter().any(|word| *word != 0),
            whole_tree_holder: holder,
            waits,
        });
        // The footprint rule can only defer a waiter (its waits are keyed by
        // the no-footprint tasks) or, while something holds the tree, an
        // editing candidate; without either the wave skips asking it.
        let mut footprint_applies =
            footprint.holds_tree() || kept.iter().any(|candidate| self.no_footprint[*candidate]);
        scratch.wave.clear();
        scratch.resource_held = false;
        let previous_head = scratch.gap_head.take();
        let host = OnceCell::new();
        let mut gap_fit = self.resource_fit.as_ref().map(|fit| {
            PassFit::new(
                fit,
                &host,
                previous_head.as_ref(),
                self.forecast_at,
                at_hours,
            )
        });
        let first = selected.len();
        for &candidate in &kept {
            let task = &self.tasks[candidate];
            if self.no_footprint[candidate] && footprint.is_awaiting(task) {
                wait(candidate, ForecastWait::Footprint);
                continue;
            }
            if selected.len() - first >= free_slots {
                wait(candidate, ForecastWait::Capacity);
                continue;
            }
            let edits = self.edits[candidate];
            if footprint_applies && footprint.defers(task, edits).is_some() {
                wait(candidate, ForecastWait::Footprint);
                continue;
            }
            if self.held[candidate].intersects(running)
                || self.surface[candidate].intersects(&scratch.wave)
            {
                wait(candidate, ForecastWait::Lock);
                continue;
            }
            if let Some(gap_fit) = gap_fit.as_mut()
                && !gap_fit.admits(&self.tasks, candidate, running, admitted_at, at_hours)
            {
                scratch.resource_held = true;
                wait(candidate, ForecastWait::Resource);
                continue;
            }
            scratch.wave.insert(candidate);
            footprint.selected(task, edits);
            footprint_applies = footprint_applies || footprint.holds_tree();
            selected.push(candidate);
        }
        kept.clear();
        scratch.kept = kept;
        if let Some(gap_fit) = gap_fit {
            scratch.gap_head = gap_fit
                .finish(|task_id| ready.iter().any(|index| self.tasks[*index].id == task_id));
        }
    }

    /// The unknown-footprint waits of `candidates` at simulated time: a
    /// pilot wait until its captured deadline, then exclusive. `None` once
    /// no candidate's deadline is still ahead, when every wait is
    /// [`Self::exclusive_waits`]'s.
    fn footprint_waits_at(
        &self,
        candidates: &[usize],
        at_hours: f64,
    ) -> Option<BTreeMap<String, FootprintWait>> {
        let pending =
            |index: &usize| self.footprint_deadline[*index].is_some_and(|hours| hours > at_hours);
        if !candidates.iter().any(pending) {
            return None;
        }
        let waits = candidates
            .iter()
            .filter(|index| self.no_footprint[**index])
            .map(|index| {
                let wait = match self.footprint_deadline[*index] {
                    Some(hours) if hours > at_hours => FootprintWait::AwaitingFootprint {
                        until: self.forecast_at + Duration::seconds((hours * 3600.0) as i64),
                    },
                    _ => FootprintWait::Exclusive,
                };
                (self.tasks[*index].id.clone(), wait)
            })
            .collect();
        Some(waits)
    }

    fn whole_tree_holder(&self, running: &ForecastTaskSet) -> Option<String> {
        if !running.intersects(&self.no_footprint_set) {
            return None;
        }
        let live: BTreeSet<String> = running
            .iter()
            .map(|index| self.tasks[index].id.clone())
            .collect();
        whole_tree_holder(&live, &self.lookup)
    }
}
