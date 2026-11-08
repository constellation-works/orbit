//! What crew admission and claimed-leaf execution need from experiments
//! [ORB-23015].
//!
//! Crew admission reads an existing assignment's crew pool and installs the
//! run's experiment capture, fresh owner claim routing reads the existing
//! capture, a claimed leaf freezes its treatment's crew on the executing host,
//! and claimed-leaf authorization validates the owner's capture. `experiment`
//! owns all of that, and it reads tasks and the claimed leaf back and draws
//! through the crew pools, so a direct call from here would close a cycle.
//! This port is the way in: `experiment` implements it on
//! [`OrbitRuntime`](crate::OrbitRuntime), so composing the runtime binds it.
//! The pool value both sides name is
//! [`ExperimentCrewPool`](crate::application::contracts::ExperimentCrewPool).

use orbit_common::OrbitError;
use orbit_types::workflow::CapturedExperiments;
use serde_json::Value;

use crate::application::contracts::ExperimentCrewPool;

/// The experiment operations job code calls, implemented by `experiment`.
pub(in crate::application) trait JobExperimentPort {
    /// The crew pool of the task's existing live, crew-bearing assignment.
    /// Existing assignments only: eligibility must never enroll a task.
    /// Implemented by experiment.
    fn job_experiment_crew_pool(
        &self,
        task_id: &str,
    ) -> Result<Option<ExperimentCrewPool>, OrbitError>;

    /// The task's existing assignments as a capture, without enrolling it; a
    /// claimed leaf reads and validates its owner's capture instead.
    /// Implemented by experiment.
    fn job_existing_experiment_capture(
        &self,
        task_id: &str,
    ) -> Result<CapturedExperiments, OrbitError>;

    /// Capture the arms the run's single task applies into `input`, enrolling
    /// it on the owner; an explicit run crew disables crew-bearing arms whole.
    /// Implemented by experiment.
    fn install_job_experiment_capture(&self, input: &mut Value) -> Result<(), OrbitError>;

    /// Freeze a claimed leaf's treatment crew once on the executing host.
    /// Implemented by experiment.
    fn install_job_claimed_experiment_crew(&self, input: &mut Value) -> Result<(), OrbitError>;

    /// Refuse a missing, mismatched or invalid owner capture for claimed task
    /// `task_id`, before any dispatch can read config. Implemented by
    /// experiment.
    fn validate_job_experiment_capture(
        &self,
        capture: Option<&CapturedExperiments>,
        task_id: &str,
    ) -> Result<(), OrbitError>;
}
