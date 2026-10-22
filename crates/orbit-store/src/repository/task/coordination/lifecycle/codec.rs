use orbit_common::OrbitError;
use serde::Serialize;

use crate::contracts::{ClaimInspection, TaskCoordinationRow};

pub(in super::super) const CLAIM: &str = "distributed-execution-claim-v1";
pub(in super::super) const STATE: &str = "distributed-claim-lifecycle-v1";
/// Immutable attribution, separate from the closed settlement vocabulary older
/// lifecycle readers understand. Published in the handoff's commit transaction.
pub(super) const HANDOFF_SETTLEMENT: &str = "distributed-handoff-settlement-v1";
pub(super) const RECEIPT: &str = "distributed-claim-mutation-v1";
/// A friction mutation's recorded friction, keyed by its receipt id.
pub(super) const FRICTION: &str = "distributed-claim-friction-v1";

/// Budgeted failure releases a task takes within
/// [`RELEASE_BUDGET_WINDOW_HOURS`] before its next one blocks it instead
/// [ORB-14257]. A release is never the candidate's fault, but one that keeps
/// recurring unattended needs a human anyway.
pub(super) const RELEASE_BUDGET: usize = 2;
pub(super) const RELEASE_BUDGET_WINDOW_HOURS: i64 = 24;

/// `transient` releases for the owner's own busy store
/// (`ClaimFailure::owner_busy`) a task takes within
/// [`RELEASE_BUDGET_WINDOW_HOURS`] outside its release budget. Each follower
/// drain suppresses its host after a few busy episodes, but a task can still
/// cycle across drains and followers while the owner stays busy; past this
/// many, a busy release counts against the budget like any other `transient`
/// one, and holds the task from the drain that released it.
pub(super) const OWNER_BUSY_RELEASE_ALLOWANCE: usize = 3;

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
/// Decode one claim lifecycle ([`STATE`]) row's payload. Every lifecycle
/// decode goes through here, so a test can count what one section decodes.
pub(in super::super) fn decode_state(value: &str) -> Result<ClaimInspection, OrbitError> {
    #[cfg(test)]
    STATE_DECODES.with(|decodes| decodes.set(decodes.get() + 1));
    decode(value)
}

#[cfg(test)]
thread_local! {
    /// Claim lifecycle rows decoded on this thread.
    pub(crate) static STATE_DECODES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
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
