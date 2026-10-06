use orbit_common::OrbitError;
use serde::Serialize;

use crate::contracts::TaskCoordinationRow;

pub(super) const CLAIM: &str = "distributed-execution-claim-v1";
pub(super) const STATE: &str = "distributed-claim-lifecycle-v1";
pub(super) const RECEIPT: &str = "distributed-claim-mutation-v1";

/// Budgeted failure releases a task takes within
/// [`RELEASE_BUDGET_WINDOW_HOURS`] before its next one blocks it instead
/// [ORB-14257]. A release is never the candidate's fault, but one that keeps
/// recurring unattended needs a human anyway.
pub(super) const RELEASE_BUDGET: usize = 2;
pub(super) const RELEASE_BUDGET_WINDOW_HOURS: i64 = 24;

pub(in super::super) fn invalid(message: &str) -> OrbitError {
    OrbitError::InvalidInput(message.into())
}
pub(in super::super) fn encode<T: Serialize>(value: &T) -> Result<String, OrbitError> {
    serde_json::to_string(value).map_err(|e| OrbitError::Store(e.to_string()))
}
pub(in super::super) fn decode<T: serde::de::DeserializeOwned>(
    value: &str,
) -> Result<T, OrbitError> {
    serde_json::from_str(value).map_err(|e| OrbitError::Store(e.to_string()))
}
pub(in super::super) fn row<T: Serialize>(
    kind: &str,
    id: &str,
    value: &T,
) -> Result<TaskCoordinationRow, OrbitError> {
    Ok(TaskCoordinationRow {
        kind: kind.into(),
        row_id: id.into(),
        payload_json: encode(value)?,
    })
}
