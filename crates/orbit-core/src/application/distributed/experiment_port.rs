//! What pull admission needs from experiments [ORB-22416].
//!
//! The owner captures a claimed task's experiment assignments, with each live
//! arm's instruction and skill files read at the claim's base, in the same
//! commit as the claim. `experiment` owns that capture, and its readout reads
//! the claim receipt this module owns, so a direct call from here would close
//! a cycle. This port is the way in: `app::experiment` implements it and
//! `app::port` binds the adapter.

use orbit_common::OrbitError;
use orbit_store::contract::AdmissionShipContract;
use orbit_types::task::Task;
use orbit_types::workflow::CapturedExperiments;

/// The capture operation implemented by `experiment`.
pub(crate) trait DistributedExperimentPort {
    /// The owner's capture for a claim of `task`: its assignments, with each
    /// live arm's instruction files read at the base `ship` names. A file that
    /// cannot be admitted is captured as a refusal, so the leaf refuses the
    /// role it applies to rather than running without it. Implemented by
    /// experiment.
    fn capture_pull_experiments(
        &self,
        task: &Task,
        ship: &AdmissionShipContract,
    ) -> Result<CapturedExperiments, OrbitError>;
}
