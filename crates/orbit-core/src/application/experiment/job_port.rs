//! Experiment's side of the job-owned experiment port [ORB-23015].

use orbit_common::OrbitError;
use orbit_types::workflow::CapturedExperiments;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::contracts::ExperimentCrewPool;
use crate::application::job::JobExperimentPort;

impl JobExperimentPort for OrbitRuntime {
    fn job_experiment_crew_pool(
        &self,
        task_id: &str,
    ) -> Result<Option<ExperimentCrewPool>, OrbitError> {
        self.experiment_crew_pool(task_id)
    }

    fn job_existing_experiment_capture(
        &self,
        task_id: &str,
    ) -> Result<CapturedExperiments, OrbitError> {
        self.existing_task_experiment_capture(task_id)
    }

    fn install_job_experiment_capture(&self, input: &mut Value) -> Result<(), OrbitError> {
        self.install_experiment_capture(input)
    }

    fn install_job_claimed_experiment_crew(&self, input: &mut Value) -> Result<(), OrbitError> {
        self.install_claimed_experiment_crew(input)
    }

    fn validate_job_experiment_capture(
        &self,
        capture: Option<&CapturedExperiments>,
        task_id: &str,
    ) -> Result<(), OrbitError> {
        super::validate_experiment_capture(capture, task_id)
    }
}
