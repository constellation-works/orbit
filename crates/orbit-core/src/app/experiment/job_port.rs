//! Experiment's side of the job-owned experiment port [ORB-23015].

use orbit_common::OrbitError;
use orbit_types::workflow::CapturedExperiments;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::contracts::ExperimentCrewPool;
use crate::application::job::JobExperimentPort;

pub(crate) struct JobPort<'a>(pub(crate) &'a OrbitRuntime);

impl JobExperimentPort for JobPort<'_> {
    fn job_experiment_crew_pool(
        &self,
        task_id: &str,
    ) -> Result<Option<ExperimentCrewPool>, OrbitError> {
        super::run_policy::experiment_crew_pool(self.0, task_id)
    }

    fn job_existing_experiment_capture(
        &self,
        task_id: &str,
    ) -> Result<CapturedExperiments, OrbitError> {
        super::capture::existing_task_experiment_capture(self.0, task_id)
    }

    fn install_job_experiment_capture(&self, input: &mut Value) -> Result<(), OrbitError> {
        super::capture::install_experiment_capture(self.0, input)
    }

    fn install_job_experiment_memory(&self, input: &mut Value) -> Result<(), OrbitError> {
        let model = input
            .get("crew")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|crew| !crew.is_empty())
            .and_then(|crew| {
                orbit_types::identity::resolve_crew(crew, self.0.settings().crews()).ok()
            })
            .map(|crew| crew.assignment.model);
        super::capture::install_experiment_memory_capture(self.0, input, model.as_deref())
    }

    fn install_job_claimed_experiment_crew(&self, input: &mut Value) -> Result<(), OrbitError> {
        super::run_policy::install_claimed_experiment_crew(self.0, input)
    }

    fn validate_job_experiment_capture(
        &self,
        capture: Option<&CapturedExperiments>,
        task_id: &str,
    ) -> Result<(), OrbitError> {
        super::validate_claimed_experiment_capture(capture, task_id)
    }
}
