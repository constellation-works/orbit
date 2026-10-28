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
//!    of its root group; the first [`super::surface_reservation::MAX_SURFACE_RESERVATIONS`] such
//!    [`super::surface_reservation::reserves_surface`] tasks reserve their surface, and the shared
//!    reservation policy withholds lower-ranked overlapping work.
//! 2. Unknown-footprint work goes through the shared [`super::unknown_footprint::FootprintWave`].
//! 3. The wave fills the free slots with pairwise non-overlapping candidates.
//! 4. Under `enforce` auto-concurrency, gap fit then judges each candidate
//!    against the simulated host load (`resource_fit.rs`) [ORB-18689].
//!
//! Nothing here reads a store after [`crate::OrbitRuntime::forecast_admission`]
//! returns.

mod generated;
mod model;
mod pass;
mod resource_fit;
mod snapshot;
mod task_set;

pub(crate) use model::ForecastAdmission;
pub(crate) use model::ForecastPassScratch;
pub(crate) use model::ForecastWait;
pub(crate) use resource_fit::{ForecastHostEvidence, RESOURCE_RECHECK_HOURS};
pub(crate) use task_set::ForecastTaskSet;
