//! CI-sweep admission's side of the task-owned port [ORB-17936].

use orbit_engine::DispatchError;
use orbit_types::task::Task;
use serde_json::Value;

use super::ci_failure::admission;
use crate::OrbitRuntime;
use crate::application::task::{CiSweepAdmission, TaskSweepFilingPort};

impl TaskSweepFilingPort for OrbitRuntime {
    fn ci_sweep_admission(
        &self,
        action: &str,
        task_id: &str,
        task: &Task,
        assessment: &Value,
        selectors: &[String],
        filing: &Value,
        promotion_authorized: bool,
        dependencies: &[String],
        is_done: &dyn Fn(&str) -> bool,
    ) -> Result<CiSweepAdmission, DispatchError> {
        admission::assess(
            action,
            task_id,
            task,
            assessment,
            selectors,
            filing,
            promotion_authorized,
            dependencies,
            is_done,
        )
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
