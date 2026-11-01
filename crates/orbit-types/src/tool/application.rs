//! Host-issued, workspace-bound application consent. No capability elevation.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Consent record schema whose every scope is exact (`input`), unnamed and
/// unlimited. Older binaries honor it, so the writer keeps emitting it for
/// such consents.
pub const APPLICATION_CONSENT_SCHEMA_V1: u32 = 1;

/// Consent record schema carrying a constrained (`fields`), named or
/// rate-limited scope. Binaries that read only v1 refuse it at admission.
pub const APPLICATION_CONSENT_SCHEMA_V2: u32 = 2;

/// One operation the operator approved, in exactly one of two forms:
/// `input`, an exact JSON object compared structurally (omitted fields,
/// additional fields and different values are different calls), or
/// `fields`, a closed map from field name to an [`ApplicationConstraint`].
/// Consent validation refuses both or neither.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationOperation {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<BTreeMap<String, ApplicationConstraint>>,
    /// Required for plugin tools, forbidden for built-ins.
    pub plugin: Option<ApplicationPluginConsent>,
}

impl ApplicationOperation {
    /// Whether this operation uses the constrained `fields` form.
    pub fn is_constrained(&self) -> bool {
        self.fields.is_some()
    }
}

/// A typed constraint on one field of a constrained operation. On the wire it
/// is a JSON object with exactly one key naming the kind.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationConstraint {
    /// A structurally equal value.
    Const(Value),
    /// A value equal to one element (1-16 elements).
    OneOf(Vec<Value>),
    /// Bounded free text.
    Text(ApplicationTextConstraint),
    /// A short ASCII token over `[A-Za-z0-9._:-]`.
    Token(ApplicationTokenConstraint),
    /// 64 lowercase hex characters.
    Sha256(ApplicationEmptyConstraint),
    /// A selector resolving to the consent's workspace.
    Workspace(ApplicationEmptyConstraint),
    /// The id of a task whose current state satisfies the predicate.
    Task(ApplicationTaskPredicate),
    /// A status transition of the task this call names.
    Transition(ApplicationTransition),
    /// The entry call's admitted value for the named field (callbacks only).
    Bound(String),
    /// JSON inside the entry-bound task's artifact (callbacks only).
    ArtifactJson(ApplicationArtifactJson),
    /// An array of distinct strings, each in the set (1-32 strings).
    SubsetOf(Vec<String>),
    /// Absence, or a value satisfying the inner constraint.
    Optional(Box<ApplicationConstraint>),
}

impl ApplicationConstraint {
    /// Stable kind name, as written on the wire.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Const(_) => "const",
            Self::OneOf(_) => "one_of",
            Self::Text(_) => "text",
            Self::Token(_) => "token",
            Self::Sha256(_) => "sha256",
            Self::Workspace(_) => "workspace",
            Self::Task(_) => "task",
            Self::Transition(_) => "transition",
            Self::Bound(_) => "bound",
            Self::ArtifactJson(_) => "artifact_json",
            Self::SubsetOf(_) => "subset_of",
            Self::Optional(_) => "optional",
        }
    }

    /// The constraint an `optional` wraps, or this constraint itself.
    pub fn required_form(&self) -> &Self {
        match self {
            Self::Optional(inner) => inner,
            other => other,
        }
    }
}

/// `{}`: a constraint kind with no parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationEmptyConstraint {}

/// `text` parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationTextConstraint {
    pub max_chars: u32,
}

/// `token` parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationTokenConstraint {
    pub max_len: u32,
}

/// `task` predicate over a task's current tags, status and artifact digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationTaskPredicate {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags_all: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags_any: Vec<String>,
    pub status: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_sha256: Option<ApplicationArtifactDigest>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pin: bool,
}

/// Binds a task artifact's stored-bytes digest to a `sha256` call field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationArtifactDigest {
    pub path: String,
    pub field: String,
}

/// `transition` parameters: the call's status is `to`, and the named task's
/// current status is in `from`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationTransition {
    pub from: Vec<String>,
    pub to: String,
}

/// `artifact_json` parameters: an RFC 6901 pointer into a bound artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationArtifactJson {
    pub path: String,
    pub pointer: String,
}

/// Optional per-scope admission rate limit: at most `admissions` admitted
/// entry calls per `window_seconds`. Absent means no limit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationRateLimit {
    pub admissions: u32,
    pub window_seconds: u32,
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
    /// Optional on an exact scope, required on a constrained one;
    /// `[a-z0-9-]{1,48}` and unique within the consent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub operation: ApplicationOperation,
    #[serde(default)]
    pub callbacks: Vec<ApplicationOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<ApplicationRateLimit>,
}

impl ApplicationScope {
    /// Whether this scope uses anything only schema v2 can carry.
    pub fn requires_schema_v2(&self) -> bool {
        self.name.is_some()
            || self.rate_limit.is_some()
            || self.operation.is_constrained()
            || self
                .callbacks
                .iter()
                .any(ApplicationOperation::is_constrained)
    }
}

/// Request presented to the host's operator-consent API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationConsentRequest {
    pub application_id: String,
    pub expires_at: DateTime<Utc>,
    pub scopes: Vec<ApplicationScope>,
}

impl ApplicationConsentRequest {
    /// The record schema this request is written as: v1 while every scope is
    /// exact, unnamed and unlimited, v2 otherwise.
    pub fn schema_version(&self) -> u32 {
        if self.scopes.iter().any(ApplicationScope::requires_schema_v2) {
            APPLICATION_CONSENT_SCHEMA_V2
        } else {
            APPLICATION_CONSENT_SCHEMA_V1
        }
    }
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
