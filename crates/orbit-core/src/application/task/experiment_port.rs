//! What task reads need from experiments [ORB-23015].
//!
//! Task show projects a task's stored experiment assignments, and the
//! provider-limit gate asks whether an existing assignment supplies the
//! task's crew pool before treating its crew as the task's own choice.
//! `experiment` owns both reads, and it reads tasks, their crew source and the
//! claimed leaf back, so a direct call from here would close a cycle. This
//! port is the way in: `app::experiment` implements it and
//! `app::port` binds the adapter.
//! Task keeps owning its readout and provider-limit decisions.

use orbit_common::OrbitError;
use orbit_store::contract::ExperimentAssignment;

/// The experiment reads task code calls, implemented by `experiment`.
pub(crate) trait TaskExperimentPort {
    /// The task's stored experiment assignments, including surface-conflict
    /// exclusions, as task show projects them. Implemented by experiment.
    fn task_experiment_assignment_readout(
        &self,
        task_id: &str,
    ) -> Result<Vec<ExperimentAssignment>, OrbitError>;

    /// Whether an existing live assignment's arm supplies the task's crew
    /// pool. Reads existing assignments only: it never enrolls the task.
    /// Implemented by experiment.
    fn task_has_experiment_crew_pool(&self, task_id: &str) -> Result<bool, OrbitError>;
}
