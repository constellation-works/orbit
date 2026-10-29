//! Host-owned assistance settings. A checkout cannot activate outbound data.

use orbit_common::OrbitError;
use orbit_types::assistance::{self, AssistanceBudget, AssistanceLimits, AssistanceMode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Whole host-owned `[assistance]` setting; an empty policy list is off.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AssistanceSettings {
    /// Bounds shared by every assisted use case on this host/store.
    pub limits: AssistanceLimits,
    /// Explicit allocation and verified input price; unfunded by default.
    pub budget: AssistanceBudget,
    /// Workspace/use-case grants. No built-in consumers are activated.
    pub policies: Vec<AssistancePolicy>,
}

/// Operator grant for one consumer's immutable question/evidence contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistancePolicy {
    /// Registered workspace identity, supplied by the owning application.
    pub workspace_id: String,
    /// Consumer name; taxonomy and evidence collection remain in its owner.
    pub use_case: String,
    /// Off enqueues nothing; shadow persists privately; advisory permits reads.
    #[serde(default)]
    pub mode: AssistanceMode,
    /// Model requested at the fixed OpenAI Decisions endpoint.
    pub model: String,
    /// Operator revision/cache epoch; advance after provider model changes.
    pub model_revision: String,
    /// Version of the evaluated consumer policy, including its thresholds.
    pub policy_version: String,
    /// SHA-256 of the serialized generic contract, including exact questions.
    pub contract_digest: String,
    /// Only these packet fields may leave the host. Others are omitted.
    pub evidence_fields: Vec<String>,
    /// Allowed prefixes of evidence references. References are never fetched.
    pub evidence_ref_prefixes: Vec<String>,
    /// Host environment variable name only; its value is never serialized.
    pub credential_env: String,
}

impl AssistanceSettings {
    /// Admit budgets, bounds and exact workspace grants without resolving secrets.
    pub fn validate_assistance_settings(&self) -> Result<(), OrbitError> {
        let invalid = || {
            OrbitError::InvalidInput("invalid host assistance settings: check bounds, allocation, contract and workspace grants".into())
        };
        let invalid_digest = || {
            OrbitError::InvalidInput("invalid host assistance settings: contract_digest must be 'sha256:' followed by 64 lowercase hex digits".into())
        };
        self.limits.validate_limits().map_err(|_| invalid())?;
        if !assistance::assistance_identifier(&self.budget.epoch)
            || self.budget.token_ceiling > i64::MAX as u64
            || self.budget.spend_ceiling_microusd > i64::MAX as u64
            || self
                .budget
                .input_microusd_per_million_tokens
                .is_some_and(|n| n == 0 || n > 1_000_000_000)
            || self
                .budget
                .pricing_version
                .as_ref()
                .is_some_and(|v| !assistance::assistance_identifier(v))
            || self.policies.len() > 128
        {
            return Err(invalid());
        }
        let mut seen = BTreeSet::new();
        for policy in &self.policies {
            if !assistance::assistance_is_digest(&policy.contract_digest) {
                return Err(invalid_digest());
            }
            if !seen.insert((&policy.workspace_id, &policy.use_case))
                || [
                    &policy.workspace_id,
                    &policy.use_case,
                    &policy.model_revision,
                    &policy.policy_version,
                ]
                .iter()
                .any(|s| !assistance::assistance_identifier(s))
                || policy.model != "gpt-6-luna"
                || policy.evidence_fields.is_empty()
                || policy.evidence_fields.len() > 128
                || policy
                    .evidence_fields
                    .iter()
                    .any(|s| !assistance::assistance_identifier(s))
                || policy.evidence_ref_prefixes.len() > 128
                || policy
                    .evidence_ref_prefixes
                    .iter()
                    .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
                || !policy.credential_env.starts_with("ORBIT_ASSISTANCE_")
                || policy.credential_env.len() > 128
                || !policy
                    .credential_env
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                || policy.credential_env.as_bytes()[0].is_ascii_digit()
            {
                return Err(invalid());
            }
        }
        Ok(())
    }

    /// Look up a host grant without taking any defaults from a workspace.
    pub fn assistance_policy(&self, workspace: &str, use_case: &str) -> Option<&AssistancePolicy> {
        self.policies
            .iter()
            .find(|p| p.workspace_id == workspace && p.use_case == use_case)
    }
}

pub(crate) fn resolve_assistance_settings(
    raw: Option<AssistanceSettings>,
) -> Result<AssistanceSettings, OrbitError> {
    let settings = raw.unwrap_or_default();
    settings.validate_assistance_settings()?;
    Ok(settings)
}
