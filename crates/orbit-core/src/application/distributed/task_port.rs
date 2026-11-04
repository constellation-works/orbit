//! Distributed crew accounting's side of the task-owned port [ORB-17936].

use orbit_types::workflow::PipelineState;

use super::auth_failures::pilot_run_crew;
use crate::OrbitRuntime;
use crate::application::task::TaskDistributedPort;

impl TaskDistributedPort for OrbitRuntime {
    fn pilot_assessment_crew(&self, state: &PipelineState) -> Option<String> {
        pilot_run_crew(state)
    }

    fn external_merge_completion_authority(&self) -> Option<String> {
        self.owner_completion_authority()
    }
}
