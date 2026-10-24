//! The auto drain's side of the job-owned auto-concurrency port
//! [ORB-17938].

use orbit_common::OrbitError;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::job::JobAutoConcurrencyPort;

impl JobAutoConcurrencyPort for OrbitRuntime {
    fn install_auto_concurrency(
        &self,
        job_name: &str,
        input: &mut Value,
        retain_snapshot: bool,
    ) -> Result<(), OrbitError> {
        self.capture_auto_concurrency(job_name, input, retain_snapshot)
    }
}
