use super::{AssistanceReason, assistance_identifier};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistanceLimits {
    pub concurrency: u32,
    pub queue_capacity: u32,
    pub max_records: u32,
    pub queue_age_seconds: u32,
    pub timeout_seconds: u32,
    pub retry_delay_seconds: u32,
    pub max_attempts: u32,
    pub max_request_bytes: u32,
    pub max_response_bytes: u32,
    pub max_input_tokens: u32,
    pub retention_seconds: u32,
}

impl Default for AssistanceLimits {
    fn default() -> Self {
        Self {
            concurrency: 2,
            queue_capacity: 128,
            max_records: 1024,
            queue_age_seconds: 300,
            timeout_seconds: 15,
            retry_delay_seconds: 5,
            max_attempts: 2,
            max_request_bytes: 16_384,
            max_response_bytes: 65_536,
            max_input_tokens: 32_768,
            retention_seconds: 604_800,
        }
    }
}

impl AssistanceLimits {
    pub fn validate_limits(&self) -> Result<(), AssistanceReason> {
        if !(1..=64).contains(&self.concurrency)
            || !(1..=10_000).contains(&self.queue_capacity)
            || !(self.queue_capacity..=100_000).contains(&self.max_records)
            || !(1..=120).contains(&self.timeout_seconds)
            || !(self.timeout_seconds..=86_400).contains(&self.queue_age_seconds)
            || !(1..=300).contains(&self.retry_delay_seconds)
            || !(1..=5).contains(&self.max_attempts)
            || !(1024..=65_536).contains(&self.max_request_bytes)
            || !(1024..=1_048_576).contains(&self.max_response_bytes)
            || !(1024..=262_144).contains(&self.max_input_tokens)
            || !(1..=2_592_000).contains(&self.retention_seconds)
        {
            return Err(AssistanceReason::InvalidRequest);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistanceBudget {
    /// Host-owned allocation identity; reservations survive retention/restarts.
    pub epoch: String,
    pub token_ceiling: u64,
    pub spend_ceiling_microusd: u64,
    /// Uncached input price ceiling. Absent pricing refuses a paid request.
    pub input_microusd_per_million_tokens: Option<u64>,
    pub pricing_version: Option<String>,
}

impl Default for AssistanceBudget {
    fn default() -> Self {
        Self {
            epoch: "unfunded".into(),
            token_ceiling: 0,
            spend_ceiling_microusd: 0,
            input_microusd_per_million_tokens: None,
            pricing_version: None,
        }
    }
}

impl AssistanceBudget {
    pub fn assistance_charge_bound(&self, tokens: u64) -> Option<u64> {
        if self
            .pricing_version
            .as_ref()
            .is_none_or(|s| !assistance_identifier(s))
        {
            return None;
        }
        tokens
            .checked_mul(self.input_microusd_per_million_tokens?)?
            .checked_add(999_999)
            .map(|n| n / 1_000_000)
    }
}
