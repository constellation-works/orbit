//! State automation's side of the job-owned port [ORB-17938].

use orbit_common::OrbitError;
use orbit_types::workflow::JobRun;
use serde_json::Value;

use super::members::bundle_crews;
use super::preparation::active_task_pilot_preparations;
use super::source::Source;
use crate::OrbitRuntime;
use crate::application::job::JobAutomationPort;
use crate::runtime::engine::crew::BundleCrews;

impl JobAutomationPort for OrbitRuntime {
    fn run_bundle_crews(&self, run: &JobRun, input: &Value) -> BundleCrews {
        bundle_crews(self, run, input)
    }

    fn source_repository(&self) -> Option<String> {
        Source::new(&self.paths().repo_root).repository().ok()
    }

    fn pilot_routine_warning(&self) -> Option<String> {
        self.task_pilot_warning()
    }

    fn pilot_holder_run(&self, task_id: &str) -> Result<Option<String>, OrbitError> {
        Ok(active_task_pilot_preparations(self)?
            .remove(task_id)
            .and_then(|holders| holders.into_iter().next()))
    }
}
