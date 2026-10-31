use orbit_common::OrbitError;
use serde::Deserialize;

use super::keys::{PRE_LANDING_REVIEW_CREW_KEY, REVIEW_CREW_KEY};
use super::layer::OperationLayerSource;

/// `workflow.purpose_crews.review` or `workflow.purpose_crews.pre_landing_review`
/// as written: one crew name, or a pool of `name[:weight]` entries
/// [ORB-15195]. A single name is a one-member pool.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ReviewCrewSetting {
    /// `review = "sol"`.
    One(String),
    /// `review = ["sol", "grok"]`.
    Pool(Vec<String>),
}

/// One review layer's crew pool and the key and layer that supplied it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewCrewPool<'a> {
    /// Canonical `name[:weight]` entries; empty when no key names a crew.
    pub entries: &'a [String],
    /// The key the entries came from, or that a fallback ended at.
    pub key: &'static str,
    /// The layer that set `key`.
    pub source: OperationLayerSource,
}

impl ReviewCrewPool<'_> {
    /// Whether the pool came from `workflow.purpose_crews.review` because
    /// `workflow.purpose_crews.pre_landing_review` names no crew.
    pub fn is_fallback(&self) -> bool {
        self.key == REVIEW_CREW_KEY
    }
}

/// The pool one layer's `workflow.purpose_crews.review` states. An empty name is
/// refused as before; an empty array states no crew. Entry grammar and crew
/// names are admitted against the registry with the rest of the snapshot.
pub(crate) fn review_crew(
    raw: Option<ReviewCrewSetting>,
) -> Result<Option<Vec<String>>, OrbitError> {
    crew_setting(raw, REVIEW_CREW_KEY)
}

/// The pool one layer's `workflow.purpose_crews.pre_landing_review` states,
/// admitted exactly as [`review_crew`] admits its key.
pub(crate) fn pre_landing_review_crew(
    raw: Option<ReviewCrewSetting>,
) -> Result<Option<Vec<String>>, OrbitError> {
    crew_setting(raw, PRE_LANDING_REVIEW_CREW_KEY)
}

fn crew_setting(
    raw: Option<ReviewCrewSetting>,
    key: &str,
) -> Result<Option<Vec<String>>, OrbitError> {
    match raw {
        Some(ReviewCrewSetting::One(value)) if value.trim().is_empty() => {
            Err(OrbitError::InvalidInput(format!("{key} must not be empty")))
        }
        Some(ReviewCrewSetting::One(value)) => Ok(Some(vec![value.trim().to_string()])),
        Some(ReviewCrewSetting::Pool(entries)) => Ok(Some(entries)),
        None => Ok(None),
    }
}
