use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_types::task::Task;
use resource_fit::ResourceFit;

use crate::application::admission::unknown_footprint::FootprintWait;

use super::resource_fit;
use super::task_set::ForecastTaskSet;

/// Why a pending task did not start at a pass: the first gate that held it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForecastWait {
    /// An in-forecast dependency has not finished.
    Dependency,
    /// Its earliest scheduled start is still ahead of simulated time.
    Scheduled,
    /// Its surface overlaps a running task's lock, its root group's does, or
    /// a task selected earlier in the same wave.
    Lock,
    /// A higher-ranked lock-blocked task reserved an overlapping surface.
    Reservation,
    /// It declares no footprint: it waits for its pilot, or for the tree.
    Footprint,
    /// Every leaf slot is taken.
    Capacity,
    /// Gap fit held it: its profile does not fit beside the running work.
    Resource,
}

impl ForecastWait {
    pub(crate) const ALL: [Self; 7] = [
        Self::Dependency,
        Self::Scheduled,
        Self::Lock,
        Self::Reservation,
        Self::Footprint,
        Self::Capacity,
        Self::Resource,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Dependency => "dependency",
            Self::Scheduled => "scheduled",
            Self::Lock => "lock",
            Self::Reservation => "reservation",
            Self::Footprint => "footprint",
            Self::Capacity => "capacity",
            Self::Resource => "resource",
        }
    }
}

/// Frozen admission inputs for one forecast. Task `i` is `tasks[i]` as the
/// caller passed it, so the forecast's own task list indexes it directly.
pub(crate) struct ForecastAdmission {
    pub(super) tasks: Vec<Task>,
    pub(super) lookup: BTreeMap<String, Task>,
    pub(super) order: Vec<usize>,
    /// `held[a]` holds `b` when `a`'s surface overlaps what `b` locks while
    /// it runs (nothing for `no-diff-expected`).
    pub(super) held: Vec<ForecastTaskSet>,
    /// `surface[a]` holds `b` when `a`'s surface overlaps `b`'s declared
    /// surface: the same-wave and reservation checks, which do not ask
    /// whether `b` will hold a lock.
    pub(super) surface: Vec<ForecastTaskSet>,
    pub(super) edits: Vec<bool>,
    pub(super) no_footprint: Vec<bool>,
    /// Hours from the forecast until which a task pilot may still prepare
    /// a no-footprint task's selectors.
    pub(super) footprint_deadline: Vec<Option<f64>>,
    /// Every no-footprint task, exclusive: the waits once no pilot
    /// deadline is ahead.
    pub(super) exclusive_waits: BTreeMap<String, FootprintWait>,
    pub(super) no_footprint_set: ForecastTaskSet,
    pub(super) root: Vec<usize>,
    pub(super) roots: usize,
    pub(super) reserves: Vec<bool>,
    pub(super) expiring: BTreeMap<String, DateTime<Utc>>,
    pub(super) forecast_at: DateTime<Utc>,
    pub(super) surface_hash: Vec<String>,
    pub(super) review_lock_overlaps: Vec<bool>,
    /// Gap fit's inputs, when the drain enforces it on a host the forecast
    /// could size.
    pub(super) resource_fit: Option<ResourceFit>,
}

/// Reusable per-pass buffers, so a simulated pass allocates only when a
/// reservation or unknown footprint is in play.
#[derive(Default)]
pub(crate) struct ForecastPassScratch {
    pub(super) direct: Vec<bool>,
    pub(super) blocked_roots: Vec<bool>,
    pub(super) kept: Vec<usize>,
    pub(super) reserving: Vec<usize>,
    pub(super) wave: ForecastTaskSet,
    /// Gap fit's head, carried between passes as the drain carries it.
    pub(super) gap_head: Option<orbit_types::workflow::GapFitHead>,
    /// Whether the last pass's gap fit held a candidate.
    pub(super) resource_held: bool,
}

impl ForecastPassScratch {
    /// Forget the previous simulated drain's head.
    pub(crate) fn start_drain(&mut self) {
        self.gap_head = None;
        self.resource_held = false;
    }

    /// Whether the last pass's gap fit held a candidate.
    pub(crate) fn resource_held(&self) -> bool {
        self.resource_held
    }
}
