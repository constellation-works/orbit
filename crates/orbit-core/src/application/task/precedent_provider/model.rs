//! Versioned, provider-neutral wire records. Citations are inert evidence.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const PRECEDENT_PROVIDER_CONTRACT: &str = "orbit.precedent-provider/1";
pub const MAX_PRECEDENT_K: usize = 32;
pub const MAX_PRECEDENT_REQUEST_BYTES: usize = 65_536;
pub const MAX_PRECEDENT_RESPONSE_BYTES: usize = 262_144;
pub const MAX_PRECEDENT_STRING_BYTES: usize = 8_192;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentProviderRequest {
    pub contract: String,
    pub workspace_id: String,
    pub request: PrecedentQuery,
    pub as_of: DateTime<Utc>,
    pub k: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentQuery {
    pub id: String,
    pub gate: String,
    pub category: String,
    pub question: String,
    pub options: Vec<PrecedentOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    pub raised_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consequence: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentProviderResponse {
    pub contract: String,
    pub precedents: Vec<DecisionPrecedent>,
    pub omissions: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionPrecedent {
    pub id: String,
    pub question: String,
    pub options: Vec<PrecedentOption>,
    pub chosen_option: String,
    pub context: String,
    pub scope: PrecedentScope,
    pub decided_at: DateTime<Utc>,
    pub decided_by: String,
    pub decided_by_kind: PrecedentDeciderKind,
    /// Contradiction evidence only. Consumers must never count it as support.
    pub reversed: bool,
    pub citation: PrecedentProviderCitation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentScope {
    pub workspace_id: String,
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrecedentDeciderKind {
    Human,
    HumanConfirmed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentProviderCitation {
    pub source: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub digest: String,
}

/// A fixed reason, never the provider's response text or an execution error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrecedentGapReason {
    Bounds,
    InvalidShape,
    ContractMismatch,
    DuplicateId,
    FutureDecision,
    UnknownOption,
    AutomationActor,
    WorkspaceMismatch,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrecedentProviderGap {
    pub provider: String,
    pub reason: PrecedentGapReason,
}

/// Retrieval evidence for the consumer's packet/ledger. Gaps and omissions
/// must travel with records; a partial retrieval cannot support self-answering.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PrecedentRetrieval {
    pub enabled: bool,
    pub precedents: Vec<DecisionPrecedent>,
    pub provider_gaps: Vec<PrecedentProviderGap>,
    pub omissions: Vec<String>,
    pub truncated: bool,
}

/// Confirmation history seam for a human review of a model resolution.
/// The event writer and confirming human must agree. Its timestamp is the
/// availability time, preventing a later confirmation leaking into replay.
pub const PRECEDENT_HUMAN_CONFIRMED_EVENT: &str = "decision_model_confirmed";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentHumanConfirmation {
    pub request_id: String,
    pub answered_at: DateTime<Utc>,
    pub confirmed_by: String,
}
