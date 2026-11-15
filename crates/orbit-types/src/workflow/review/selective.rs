//! Removed selective before-landing review [ORB-22808]: the wire shapes an
//! older build still sends or stored.
//!
//! Nothing evaluates, produces or enforces a selective policy any more. The
//! captured policy stays a typed legacy field of the claim review contract
//! and the review admission so older followers' requests, stored claims and
//! run snapshots still parse, and so the distributed-drain protocol
//! fingerprint, derived from the request schema, does not change. Each type
//! below is kept byte-for-byte in shape for that reason; the field is
//! ignored wherever it is read. Drop these types after 2026-12-08.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// `review.selective_before_landing.mode`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SelectiveBeforeLandingMode {
    /// No selective scan; today's behavior (the default).
    #[default]
    Off,
    /// Record what would require review; never invoke or enforce.
    Observe,
    /// A positive decision requires the existing before-landing review.
    Enforce,
}

/// `review.selective_before_landing.unknown_risk`: what a configured risk
/// predicate with no usable estimate does. It never applies to an
/// unreadable path diff.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SelectiveUnknownRisk {
    /// Treat unknown risk as requiring review (the default).
    #[default]
    Review,
    /// Treat unknown risk as not selecting on the risk predicate.
    Skip,
}

/// The selective before-landing policy an older owner captured. Ignored;
/// never produced by this build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SelectiveBeforeLandingPolicy {
    pub mode: SelectiveBeforeLandingMode,
    /// Canonical decimal probability in `[0, 1]`; selection is strictly
    /// `risk > threshold`. `None` disables the risk predicate. Stored as
    /// text so the captured contract stays exactly comparable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_threshold: Option<String>,
    /// Owner-captured model identity, including coefficients or an explicit unknown.
    /// Never supplied by candidate configuration or inferred from a prior receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_model_identity: Option<String>,
    /// Positive repository-relative path globs; empty disables the path
    /// predicate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    pub unknown_risk: SelectiveUnknownRisk,
    /// Which configuration layer decided each field, by dotted key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, String>,
    /// SHA-256 of the canonical predicate configuration, so a receipt can
    /// be checked against the policy it claims to have applied.
    pub digest: String,
}
