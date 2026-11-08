//! The crew pool an experiment arm's overlay supplies, as `job` reads it
//! through `job::JobExperimentPort` [ORB-23015].
//!
//! `experiment` builds it from an existing assignment's frozen overlay; `job`
//! draws a crew from it. Neither names the other's types, so the value lives
//! here.

use orbit_config::CrewPoolEntry;

/// The canonical crew pool of a task's live, crew-bearing experiment arm.
pub(crate) struct ExperimentCrewPool {
    pub(crate) entries: Vec<CrewPoolEntry>,
    pub(crate) source: String,
    pub(crate) digest: String,
    /// The experiment and arm whose overlay this pool is, which a crew
    /// written back to the task names.
    pub(crate) experiment: String,
    pub(crate) arm: String,
}
