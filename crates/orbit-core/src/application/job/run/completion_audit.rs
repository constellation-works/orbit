//! The completion audit the live drain controls share: stop admissions, the
//! worker ceiling and the window extension. Each control keeps its own event
//! name; the arguments, the detail merge and the status mapping are one rule.

use orbit_common::OrbitError;
use orbit_types::telemetry::AuditEventStatus;
use serde_json::{Value, json};

use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Records how one drain control request ended under `event`.
    ///
    /// The arguments are the request id, run id, outcome and completion time,
    /// with `detail` merged in after them. A `rejected` outcome is a failure;
    /// every other outcome is a success.
    pub(super) fn record_drain_control_completion(
        &self,
        event: &str,
        run_id: Option<&str>,
        request_id: &str,
        outcome: &str,
        detail: Value,
        error: Option<String>,
    ) -> Result<(), OrbitError> {
        let mut arguments = json!({
            "request_id": request_id,
            "run_id": run_id,
            "outcome": outcome,
            "completed_at": chrono::Utc::now().to_rfc3339(),
        });
        if let (Some(target), Some(detail)) = (arguments.as_object_mut(), detail.as_object()) {
            for (key, value) in detail {
                target.insert(key.clone(), value.clone());
            }
        }
        self.record_pipeline_audit(
            event,
            run_id,
            None,
            if outcome == "rejected" {
                AuditEventStatus::Failure
            } else {
                AuditEventStatus::Success
            },
            arguments,
            error,
        )
    }
}
