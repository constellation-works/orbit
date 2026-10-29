//! Durable assistance ledger. These records confer no workflow authority.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::assistance::{
    AssistanceBudget, AssistanceLimits, AssistanceMode, AssistanceRequest, AssistanceResult,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistanceWork {
    pub id: String,
    pub slot: String,
    pub policy_digest: String,
    pub evidence_digest: String,
    pub request: AssistanceRequest,
    pub adapter: String,
    pub requested_model: String,
    pub model_revision: String,
    pub mode: AssistanceMode,
    pub created_at: DateTime<Utc>,
    pub limits: AssistanceLimits,
    pub budget: AssistanceBudget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistanceClaim {
    pub work: AssistanceWork,
    pub attempt_id: String,
    pub ordinal: u32,
    pub deadline: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistanceAttempt {
    pub attempt_id: String,
    pub ordinal: u32,
    pub outcome_unknown: bool,
    pub reserved_tokens: u64,
    pub reserved_charge_microusd: u64,
    pub result: Option<AssistanceResult>,
    pub late_result: Option<AssistanceResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistanceRecord {
    pub work: AssistanceWork,
    pub current: bool,
    /// The row reached `done`. A queued or claimed row's `result`, when present,
    /// is only its last retry attempt's gap, not a settled answer.
    pub settled: bool,
    pub result: Option<AssistanceResult>,
    pub attempts: Vec<AssistanceAttempt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssistanceAdmission {
    Enqueued(String),
    Existing(String),
    Disabled,
    QueueFull,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssistanceAccounting {
    pub reserved_tokens: u64,
    pub reserved_charge_microusd: u64,
    pub attempts: u64,
    pub outcome_unknown_attempts: u64,
    pub queue_rejections: u64,
}

/// Calls are short transactions. A claim is committed before the caller may
/// invoke an adapter; a completion is fenced by its unique attempt and slot.
pub trait AssistanceStoreBackend: Send + Sync {
    fn assistance_enqueue(&self, work: &AssistanceWork) -> Result<AssistanceAdmission, OrbitError>;
    fn assistance_claim(
        &self,
        workspace: &str,
        use_case: &str,
        policy_digest: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<AssistanceClaim>, OrbitError>;
    fn assistance_finish(
        &self,
        claim: &AssistanceClaim,
        result: AssistanceResult,
        now: DateTime<Utc>,
    ) -> Result<bool, OrbitError>;
    /// Settles a claim refused before any outbound request. It releases the
    /// claim's reservation and never counts as an outcome-unknown attempt.
    fn assistance_finish_unattempted(
        &self,
        claim: &AssistanceClaim,
        result: AssistanceResult,
        now: DateTime<Utc>,
    ) -> Result<bool, OrbitError>;
    fn assistance_record(&self, id: &str) -> Result<Option<AssistanceRecord>, OrbitError>;
    fn assistance_accounting(&self, epoch: &str) -> Result<AssistanceAccounting, OrbitError>;
    /// Expire abandoned claims, queued work and retained terminal records.
    /// Allocation counters are deliberately not reset by retention.
    fn assistance_maintain(&self, now: DateTime<Utc>) -> Result<(), OrbitError>;
}
