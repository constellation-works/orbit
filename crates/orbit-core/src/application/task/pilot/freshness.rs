//! Reuse the output certified by an atomic pilot apply, across consumers.

use crate::OrbitRuntime;
use orbit_common::OrbitError;
use serde_json::Value;

impl OrbitRuntime {
    /// Return the latest applied assessment for automatic scheduling. An
    /// assessment remains evidence that a pilot ran even after material edits;
    /// it does not certify the edited material for dispatch.
    pub(crate) fn task_pilot_scheduling_assessment(
        &self,
        task_id: &str,
    ) -> Result<Option<bool>, OrbitError> {
        let comments = self.get_task_comments(task_id)?;
        for (_, receipt) in super::pilot_receipts(&comments) {
            if !super::is_operation_id(receipt.operation_id) {
                continue;
            }
            // Only the latest accepted receipt is relevant. In particular,
            // never fall back to an older ready verdict after a new finding.
            // An unreadable body fails closed rather than skipping to an
            // older receipt, unlike blocker warnings.
            let Ok(audit) = serde_json::from_str::<Value>(receipt.body) else {
                return Ok(None);
            };
            let Some(assessment) = audit.get("assessment").filter(|value| value.is_object()) else {
                return Ok(None);
            };
            return Ok(Some(super::member_ready(assessment)));
        }
        Ok(None)
    }
}
