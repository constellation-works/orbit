//! Host-issued, workspace-bound application consent. No capability elevation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One exact operation the operator approved. JSON equality is structural;
/// omitted fields, additional fields and different values are different calls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationOperation {
    pub tool: String,
    pub input: Value,
    /// Required for plugin tools, forbidden for built-ins.
    pub plugin: Option<ApplicationPluginConsent>,
}

/// Plugin identity and installed manifest digest approved by the operator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationPluginConsent {
    pub name: String,
    pub manifest_digest: String,
}

/// An approved entry call and its separately approved completion callbacks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationScope {
    pub operation: ApplicationOperation,
    #[serde(default)]
    pub callbacks: Vec<ApplicationOperation>,
}

/// Request presented to the host's operator-consent API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationConsentRequest {
    pub application_id: String,
    pub expires_at: DateTime<Utc>,
    pub scopes: Vec<ApplicationScope>,
}

/// Non-secret authenticated provenance, also persisted with audit events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplicationConsent {
    pub schema_version: u32,
    pub consent_id: String,
    pub application_id: String,
    pub workspace_id: String,
    pub workspace_root: String,
    pub consented_by: String,
    pub consent_provenance: String,
    pub consented_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub scopes: Vec<ApplicationScope>,
}

/// Host-minted reference passed to a plugin instead of the app credential.
/// It is authoritative only inside a verified host callback session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplicationInvocation {
    pub consent: ApplicationConsent,
    pub scope_index: usize,
    pub callback: bool,
}
