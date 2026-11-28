//! CI-sweep admission's side of the task-owned port [ORB-17936].

use orbit_engine::DispatchError;
use serde_json::Value;

use super::ci_failure::admission;
use crate::OrbitRuntime;
use crate::application::task::{CiSweepAdmission, CiSweepAdmissionRequest, TaskSweepFilingPort};

/// Adapter binding [`TaskSweepFilingPort`] to CI-sweep admission.
#[expect(dead_code, reason = "admission decides from its request alone")]
pub(crate) struct TaskPort<'a>(pub(crate) &'a OrbitRuntime);

impl TaskSweepFilingPort for TaskPort<'_> {
    fn ci_sweep_admission(
        &self,
        request: CiSweepAdmissionRequest<'_>,
    ) -> Result<CiSweepAdmission, DispatchError> {
        admission::assess_ci_sweep_admission(request)
    }

    fn ci_sweep_piloted_elsewhere(
        &self,
        action: &str,
        task_id: &str,
        prepared: &Value,
    ) -> Result<Option<Value>, DispatchError> {
        admission::piloted_elsewhere(action, task_id, prepared)
    }
}
