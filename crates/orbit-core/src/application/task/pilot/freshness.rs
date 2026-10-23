//! Reuse the output certified by an atomic pilot apply, across consumers.

use crate::OrbitRuntime;
use orbit_common::OrbitError;
use serde_json::Value;

impl OrbitRuntime {
    /// The latest applied pilot assessment, only when its post-write material
    /// equals the requesting consumer's current fingerprint. The fingerprint
    /// already includes its resolved material fields and source/policy inputs;
    /// recomputing an old assessment under a new policy would bless evidence
    /// that the pilot never certified. Missing or older markers fail closed.
    pub(crate) fn fresh_task_pilot_assessment(
        &self,
        task_id: &str,
        fingerprint: &str,
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
            let certified = audit["applied_material"]["material"]
                .get(0)
                .and_then(Value::as_str);
            if certified != Some(fingerprint) {
                return Ok(None);
            }
            return Ok(Some(super::member_ready(assessment)));
        }
        Ok(None)
    }
}
