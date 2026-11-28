//! The auto drain's side of the job-owned auto-concurrency port
//! [ORB-17938].

use orbit_common::OrbitError;
use serde_json::Value;

use super::mode;
use crate::OrbitRuntime;
use crate::application::job::JobAutoConcurrencyPort;

/// Adapter binding [`JobAutoConcurrencyPort`] to workspace auto's concurrency
/// mode operations.
pub(crate) struct JobPort<'a>(pub(crate) &'a OrbitRuntime);

impl JobAutoConcurrencyPort for JobPort<'_> {
    fn install_auto_concurrency(
        &self,
        job_name: &str,
        input: &mut Value,
        retain_snapshot: bool,
    ) -> Result<(), OrbitError> {
        mode::capture_auto_concurrency(self.0, job_name, input, retain_snapshot)
    }

    fn captured_auto_concurrency(&self, run_id: &str) -> Result<Value, OrbitError> {
        mode::run_auto_concurrency(self.0, run_id)
    }
}
