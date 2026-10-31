use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::HostEvidenceRule;
use serde::{Deserialize, Serialize};

use super::crew::{pre_landing_review_crew, review_crew};
use super::hosts::{before_landing_hosts, host_evidence_rules};
use super::keys::{
    PRE_LANDING_REVIEW_CREW_KEY, REVIEW_BEFORE_LANDING_HOSTS_KEY, REVIEW_CREW_KEY,
    REVIEW_HOST_EVIDENCE_KEY, REVIEW_MINUTES_KEY, REVIEW_TIMING_KEY, SELECTIVE_MODE_KEY,
    SELECTIVE_PATHS_KEY, SELECTIVE_UNKNOWN_RISK_KEY,
};
use super::legacy::LegacyReviewPolicy;
use super::minutes::review_minutes;
use super::selective::{
    SelectiveLayer, selective_mode_parse, selective_paths, selective_threshold,
    selective_unknown_risk_parse,
};
use super::timing::ReviewTimingSetting;
use crate::registry::read_optional;

/// Which layer supplied a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationLayerSource {
    /// The crate's built-in defaults.
    BuiltIn,
    /// The global `config.toml`.
    Global,
    /// The workspace `config.toml`.
    Workspace,
}

impl OperationLayerSource {
    /// The label captured into review admissions and shown in diagnostics.
    pub fn label(self) -> &'static str {
        match self {
            OperationLayerSource::BuiltIn => "built-in",
            OperationLayerSource::Global => "global",
            OperationLayerSource::Workspace => "workspace",
        }
    }
}

/// One resolved field with its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationField<T> {
    /// The resolved value.
    pub value: T,
    /// The layer that decided it.
    pub source: OperationLayerSource,
}

impl<T: Clone> OperationField<T> {
    pub(super) fn built_in(value: T) -> Self {
        Self {
            value,
            source: OperationLayerSource::BuiltIn,
        }
    }

    pub(super) fn set(&mut self, value: Option<&T>, layer: OperationLayerSource) {
        if let Some(value) = value {
            self.value = value.clone();
            self.source = layer;
        }
    }
}

/// The explicit review statements one layer makes. Every field is optional:
/// an omitted field inherits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OperationLayer {
    /// The before-PR half of an explicit `review.timing`, or of its
    /// translation from the deprecated switches or a legacy policy.
    pub review_before_pr: Option<bool>,
    /// The before-landing half of an explicit `review.timing`.
    pub review_before_landing: Option<bool>,
    /// Explicit `review.before_landing_hosts`; an empty list clears the
    /// machines an earlier layer listed.
    pub review_before_landing_hosts: Option<Vec<String>>,
    /// Explicit `review.minutes`, or the legacy `operation.review_minutes`.
    pub review_minutes: Option<u32>,
    /// Explicit review crew pool; a single name is a one-entry pool and an
    /// empty pool unsets a crew an earlier layer named.
    pub review_crew: Option<Vec<String>>,
    /// Explicit pre-landing review crew pool, stated like `review_crew`; an
    /// empty pool unsets one an earlier layer named, so review falls back.
    pub pre_landing_review_crew: Option<Vec<String>>,
    /// Explicit `[[review.host_evidence]]` rules; an empty list clears the
    /// rules an earlier layer stated.
    pub review_host_evidence: Option<Vec<HostEvidenceRule>>,
    /// What a legacy `operation.review_policy` says about after-landing
    /// review: `Some(true)` for `after-landing`, `Some(false)` for any other
    /// value, `None` when the layer does not state the legacy key.
    pub legacy_after_landing: Option<bool>,
    /// Explicit `[review.selective_before_landing]` statements.
    pub selective_before_landing: SelectiveLayer,
}

impl OperationLayer {
    /// Read review preferences. Unknown fields are diagnosed by the config
    /// loader and ignored so different binary versions can share a file.
    /// Known fields still validate their types, values and policy constraints.
    pub fn from_document(document: &toml::Value, config_path: &Path) -> Result<Self, OrbitError> {
        for table in ["operation", "review"] {
            if document.get(table).is_some_and(|value| !value.is_table()) {
                return Err(OrbitError::InvalidInput(format!(
                    "[{table}] must be a table"
                )));
            }
        }

        // The document is translated before it is read, so a deprecated
        // switch already reached `review.timing`; one key states both halves.
        let timing = read_optional::<String>(document, REVIEW_TIMING_KEY, config_path)?
            .map(|raw| ReviewTimingSetting::parse_in_file(&raw, config_path))
            .transpose()?
            .map(ReviewTimingSetting::switches);
        Ok(Self {
            review_before_pr: timing.map(|(before_pr, _)| before_pr),
            review_before_landing: timing.map(|(_, before_landing)| before_landing),
            review_before_landing_hosts: before_landing_hosts(read_optional(
                document,
                REVIEW_BEFORE_LANDING_HOSTS_KEY,
                config_path,
            )?)?,
            review_minutes: review_minutes(read_optional(
                document,
                REVIEW_MINUTES_KEY,
                config_path,
            )?)?,
            review_crew: review_crew(read_optional(document, REVIEW_CREW_KEY, config_path)?)?,
            pre_landing_review_crew: pre_landing_review_crew(read_optional(
                document,
                PRE_LANDING_REVIEW_CREW_KEY,
                config_path,
            )?)?,
            review_host_evidence: read_optional::<Vec<HostEvidenceRule>>(
                document,
                REVIEW_HOST_EVIDENCE_KEY,
                config_path,
            )?
            .map(host_evidence_rules)
            .transpose()?,
            legacy_after_landing: LegacyReviewPolicy::read(document, config_path)?
                .map(|policy| policy == LegacyReviewPolicy::AfterLanding),
            selective_before_landing: SelectiveLayer {
                mode: read_optional::<String>(document, SELECTIVE_MODE_KEY, config_path)?
                    .map(|raw| selective_mode_parse(&raw))
                    .transpose()?,
                risk_threshold: selective_threshold(document, config_path)?,
                paths: selective_paths(read_optional(document, SELECTIVE_PATHS_KEY, config_path)?)?,
                unknown_risk: read_optional::<String>(
                    document,
                    SELECTIVE_UNKNOWN_RISK_KEY,
                    config_path,
                )?
                .map(|raw| selective_unknown_risk_parse(&raw))
                .transpose()?,
            },
        })
    }

    /// Whether this layer states anything at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}
