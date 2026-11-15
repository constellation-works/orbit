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

    fn install_job_experiment_memory(&self, input: &mut Value) -> Result<(), OrbitError> {
        let model = input
            .get("crew")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|crew| !crew.is_empty())
            .and_then(|crew| {
                orbit_types::identity::resolve_crew(crew, self.context.settings().crews()).ok()
            })
            .map(|crew| crew.assignment.model);
        self.install_experiment_memory_capture(input, model.as_deref())
    }

    fn install_job_claimed_experiment_crew(&self, input: &mut Value) -> Result<(), OrbitError> {
        self.install_claimed_experiment_crew(input)
    }

    fn validate_job_experiment_capture(
        &self,
        capture: Option<&CapturedExperiments>,
        task_id: &str,
    ) -> Result<(), OrbitError> {
        super::validate_claimed_experiment_capture(capture, task_id)
    }
}
