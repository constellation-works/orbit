//! State automation's side of the job-owned port [ORB-17938].

use orbit_types::workflow::JobRun;
use serde_json::Value;

use super::members::bundle_crews;
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
}
