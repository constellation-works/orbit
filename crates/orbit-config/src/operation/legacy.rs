use std::path::Path;

use orbit_common::OrbitError;

use super::keys::LEGACY_REVIEW_POLICY_KEY;
use crate::registry::read_optional;

/// A retired `operation.review_policy` value, read only to translate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LegacyReviewPolicy {
    None,
    BeforePr,
    AfterLanding,
}

impl LegacyReviewPolicy {
    const CHOICES: &'static [&'static str] = &["none", "before-pr", "after-landing"];

    fn parse(raw: &str) -> Result<Self, OrbitError> {
        match raw.trim() {
            "none" => Ok(Self::None),
            "before-pr" => Ok(Self::BeforePr),
            "after-landing" => Ok(Self::AfterLanding),
            other => Err(OrbitError::InvalidInput(format!(
                "{LEGACY_REVIEW_POLICY_KEY} (deprecated) has invalid value '{other}'; expected \
                 one of: {}",
                Self::CHOICES.join(", ")
            ))),
        }
    }

    pub(super) fn read(
        document: &toml::Value,
        config_path: &Path,
    ) -> Result<Option<Self>, OrbitError> {
        read_optional::<String>(document, LEGACY_REVIEW_POLICY_KEY, config_path)?
            .map(|raw| Self::parse(&raw))
            .transpose()
    }
}

/// What a retired `operation.review_policy` says about before-PR review, or
/// `None` when the document does not state it. [`crate::aliases`] folds it
/// into `review.timing` with the document's other before-landing statements.
pub(crate) fn legacy_policy_before_pr(
    document: &toml::Value,
    config_path: &Path,
) -> Result<Option<bool>, OrbitError> {
    Ok(LegacyReviewPolicy::read(document, config_path)?
        .map(|policy| policy == LegacyReviewPolicy::BeforePr))
}
