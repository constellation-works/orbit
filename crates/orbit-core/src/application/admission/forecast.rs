//! The backlog forecast's view of drain admission [ORB-18686].
//!
//! The forecast simulates the drain without dispatching anything, so it needs
//! the drain's admission decisions over a frozen task set and simulated time.
//! [`ForecastAdmission`] freezes the inputs once per forecast: the owner's
//! dispatch order (`owner_dispatch_order`), each task's lock surface and the
//! pairwise overlaps the real lock predicates (`active_task_lock_holders`,
//! `task_lock_overlaps`) report between them, the unknown-footprint waits and
//! the surface-reservation eligibility. [`ForecastAdmission::pass`] then
//! replays one admission pass, gate by gate as `backlog_snapshot` and
//! `select_admissions` apply them, reading the precomputed rows instead of
//! expanding selectors again:
//!
//! 1. A candidate overlapping a running task's held lock waits, with the rest
//!    of its root group; the first [`MAX_SURFACE_RESERVATIONS`] such
//!    [`reserves_surface`] tasks reserve their surface, and the shared
//!    reservation policy withholds lower-ranked overlapping work.
//! 2. Unknown-footprint work goes through the shared [`FootprintWave`].
//! 3. The wave fills the free slots with pairwise non-overlapping candidates.
//!
//! Nothing here reads a store after [`OrbitRuntime::forecast_admission`]
//! returns.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Duration, Utc};
use orbit_types::task::{Task, TaskStatus, declares_no_footprint};

use super::backlog_exclusion::{
    expiring_backlog_batches, sort_tasks_for_automatic_dispatch, task_root_id,
};
use super::surface_reservation::{
    MAX_SURFACE_RESERVATIONS, reserves_surface, withhold_reserved_by,
};
use super::unknown_footprint::{
    FootprintGuard, FootprintWait, FootprintWave, footprint_waits, whole_tree_holder,
};
use crate::OrbitRuntime;
use crate::runtime::task::locks::{
    active_task_lock_holders, lock_context_files_for_task, lock_holder_index, task_lock_overlaps,
};

/// A fixed-width set of task indices.
#[derive(Clone, Default)]
pub(crate) struct ForecastTaskSet {
    words: Vec<u64>,
}

impl ForecastTaskSet {
    pub(crate) fn with_len(len: usize) -> Self {
        Self {
            words: vec![0; len.div_ceil(64)],
        }
    }

    pub(crate) fn insert(&mut self, index: usize) {
        self.words[index / 64] |= 1 << (index % 64);
    }

    pub(crate) fn remove(&mut self, index: usize) {
        self.words[index / 64] &= !(1 << (index % 64));
    }

    pub(crate) fn contains(&self, index: usize) -> bool {
        self.words[index / 64] & (1 << (index % 64)) != 0
    }

    pub(crate) fn clear(&mut self) {
        self.words.fill(0);
    }

    fn intersects(&self, other: &Self) -> bool {
        self.words
            .iter()
            .zip(&other.words)
            .any(|(left, right)| left & right != 0)
    }

    fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(word, bits)| {
            (0..64)
                .filter(move |bit| bits & (1 << bit) != 0)
                .map(move |bit| word * 64 + bit)
        })
    }
}

/// Why a pending task did not start at a pass: the first gate that held it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForecastWait {
    /// An in-forecast dependency has not finished.
    Dependency,
    /// Its surface overlaps a running task's lock, its root group's does, or
    /// a task selected earlier in the same wave.
    Lock,
    /// A higher-ranked lock-blocked task reserved an overlapping surface.
    Reservation,
    /// It declares no footprint: it waits for its pilot, or for the tree.
    Footprint,
    /// Every leaf slot is taken.
    Capacity,
}

impl ForecastWait {
    pub(crate) const ALL: [Self; 5] = [
        Self::Dependency,
        Self::Lock,
        Self::Reservation,
        Self::Footprint,
        Self::Capacity,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Dependency => "dependency",
            Self::Lock => "lock",
            Self::Reservation => "reservation",
            Self::Footprint => "footprint",
            Self::Capacity => "capacity",
        }
    }
}

/// Frozen admission inputs for one forecast. Task `i` is `tasks[i]` as the
/// caller passed it, so the forecast's own task list indexes it directly.
pub(crate) struct ForecastAdmission {
    tasks: Vec<Task>,
    lookup: BTreeMap<String, Task>,
    order: Vec<usize>,
    /// `held[a]` holds `b` when `a`'s surface overlaps what `b` locks while
    /// it runs (nothing for `no-diff-expected`).
    held: Vec<ForecastTaskSet>,
    /// `surface[a]` holds `b` when `a`'s surface overlaps `b`'s declared
    /// surface: the same-wave and reservation checks, which do not ask
    /// whether `b` will hold a lock.
    surface: Vec<ForecastTaskSet>,
    edits: Vec<bool>,
    no_footprint: Vec<bool>,
    /// Hours from the forecast until which a task pilot may still prepare
    /// a no-footprint task's selectors.
    footprint_deadline: Vec<Option<f64>>,
    /// Every no-footprint task, exclusive: the waits once no pilot
    /// deadline is ahead.
    exclusive_waits: BTreeMap<String, FootprintWait>,
    no_footprint_set: ForecastTaskSet,
    root: Vec<usize>,
    roots: usize,
    reserves: Vec<bool>,
    expiring: BTreeMap<String, DateTime<Utc>>,
    forecast_at: DateTime<Utc>,
    surface_hash: Vec<String>,
    review_lock_overlaps: Vec<bool>,
}

/// Reusable per-pass buffers, so a simulated pass allocates only when a
/// reservation or unknown footprint is in play.
#[derive(Default)]
pub(crate) struct ForecastPassScratch {
    direct: Vec<bool>,
    blocked_roots: Vec<bool>,
    kept: Vec<usize>,
    reserving: Vec<usize>,
    wave: ForecastTaskSet,
}

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

    /// A stable digest of task `index`'s canonical lock surface.
    pub(crate) fn surface_hash(&self, index: usize) -> &str {
        &self.surface_hash[index]
    }

    /// Whether task `index`'s surface overlaps a lock a `review` task holds.
    /// The forecast does not yet model review residuals, so it counts these.
    pub(crate) fn overlaps_review_lock(&self, index: usize) -> bool {
        self.review_lock_overlaps[index]
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
    /// single-slot drain applies no unknown-footprint rule.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn pass(
        &self,
        ready: &[usize],
        running: &ForecastTaskSet,
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
            scratch.wave.insert(candidate);
            footprint.selected(task, edits);
            footprint_applies = footprint_applies || footprint.holds_tree();
            selected.push(candidate);
        }
        kept.clear();
        scratch.kept = kept;
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

/// FNV-1a over the canonical selectors: a stable footprint digest for
/// stored per-task predictions.
fn surface_digest(selectors: &[String]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in selectors.join("\n").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

impl OrbitRuntime {
    /// Freeze the admission inputs for `tasks` (the forecast's initial and
    /// proposed tasks) at `now`. `workspace` is every workspace task, for
    /// root groups and the `review` locks the forecast does not model yet.
    pub(crate) fn forecast_admission(
        &self,
        tasks: Vec<Task>,
        workspace: &BTreeMap<String, Task>,
        now: DateTime<Utc>,
    ) -> ForecastAdmission {
        let workspace_root = self.paths().repo_root.as_path();
        let unlocked = self.unlocked_context_paths();
        let lock_modes = self.footprint_lock_modes();
        let len = tasks.len();
        let index_of: HashMap<&str, usize> = tasks
            .iter()
            .enumerate()
            .map(|(index, task)| (task.id.as_str(), index))
            .collect();

        // What each task would lock once admitted: the real holder rule,
        // applied as if every task were running.
        let as_running: Vec<Task> = tasks
            .iter()
            .map(|task| Task {
                status: TaskStatus::InProgress,
                ..task.clone()
            })
            .collect();
        let held_by = active_task_lock_holders(&as_running, workspace_root, unlocked);
        let mut declared: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut surfaces = Vec::with_capacity(len);
        for task in &tasks {
            let selectors = lock_context_files_for_task(task, workspace_root, unlocked);
            for selector in &selectors {
                declared
                    .entry(selector.clone())
                    .or_default()
                    .push(task.id.clone());
            }
            surfaces.push(selectors);
        }
        let review: Vec<&Task> = workspace
            .values()
            .filter(|task| task.status == TaskStatus::Review)
            .collect();
        let review_holders = active_task_lock_holders(review, workspace_root, unlocked);
        let rows = |holders: &BTreeMap<String, Vec<String>>| -> Vec<ForecastTaskSet> {
            let index = lock_holder_index(holders);
            tasks
                .iter()
                .enumerate()
                .map(|(own, task)| {
                    let mut row = ForecastTaskSet::with_len(len);
                    for overlap in
                        task_lock_overlaps(task, &index, workspace_root, unlocked, lock_modes)
                    {
                        if let Some(other) = index_of.get(overlap.locking_task_id.as_str())
                            && *other != own
                        {
                            row.insert(*other);
                        }
                    }
                    row
                })
                .collect()
        };
        let held = rows(&held_by);
        let surface = rows(&declared);
        let review_index = lock_holder_index(&review_holders);
        let review_lock_overlaps = tasks
            .iter()
            .map(|task| {
                !task_lock_overlaps(task, &review_index, workspace_root, unlocked, lock_modes)
                    .is_empty()
            })
            .collect();

        let mut root_ids: HashMap<String, usize> = HashMap::new();
        let root = tasks
            .iter()
            .map(|task| {
                let next = root_ids.len();
                *root_ids
                    .entry(task_root_id(task, workspace))
                    .or_insert(next)
            })
            .collect();

        let mut refs: Vec<&Task> = tasks.iter().collect();
        let expiring = expiring_backlog_batches(self, &refs);
        let limits = self.executor_host_os_limits().unwrap_or_else(|error| {
            tracing::warn!(%error, "backlog forecast: executor host limits unreadable");
            BTreeMap::new()
        });
        sort_tasks_for_automatic_dispatch(
            &mut refs,
            &expiring,
            &self.owner_dispatch_order(&limits),
        );
        let order = refs.iter().map(|task| index_of[task.id.as_str()]).collect();

        let no_footprint: Vec<bool> = tasks
            .iter()
            .map(|task| declares_no_footprint(&task.tags, &task.context_files))
            .collect();
        let waiting = tasks
            .iter()
            .filter(|task| matches!(task.status, TaskStatus::Backlog | TaskStatus::Proposed));
        let waits = footprint_waits(self, "backlog_forecast", waiting, now).unwrap_or_else(|error| {
            tracing::warn!(%error, "backlog forecast: footprint waits unreadable; no pilot wait");
            BTreeMap::new()
        });
        let footprint_deadline = tasks
            .iter()
            .map(|task| match waits.get(&task.id) {
                Some(FootprintWait::AwaitingFootprint { until }) => {
                    Some((*until - now).num_seconds().max(0) as f64 / 3600.0)
                }
                _ => None,
            })
            .collect();

        let exclusive_waits = tasks
            .iter()
            .zip(&no_footprint)
            .filter(|(_, none)| **none)
            .map(|(task, _)| (task.id.clone(), FootprintWait::Exclusive))
            .collect();
        let mut no_footprint_set = ForecastTaskSet::with_len(len);
        for (index, none) in no_footprint.iter().enumerate() {
            if *none {
                no_footprint_set.insert(index);
            }
        }
        ForecastAdmission {
            exclusive_waits,
            no_footprint_set,
            lookup: tasks
                .iter()
                .map(|task| (task.id.clone(), task.clone()))
                .collect(),
            order,
            held,
            surface,
            edits: surfaces
                .iter()
                .map(|selectors| !selectors.is_empty())
                .collect(),
            no_footprint,
            footprint_deadline,
            roots: root_ids.len(),
            root,
            reserves: tasks.iter().map(reserves_surface).collect(),
            expiring,
            forecast_at: now,
            surface_hash: surfaces
                .iter()
                .map(|selectors| surface_digest(selectors))
                .collect(),
            review_lock_overlaps,
            tasks,
        }
    }
}
