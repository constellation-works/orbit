//! The deterministic observer's durable live-cycle fence.

/// A live cycle published by the deterministic observer after taking its lease.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidentLiveCycle {
    pub cycle: String,
    pub run_id: String,
    /// The cycle lease's expiry; an expired writer is never live.
    pub expires_at: chrono::DateTime<chrono::Utc>,
}
