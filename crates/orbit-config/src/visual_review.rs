//! `[visual_review]`: the view manifests mock names are validated
//! against, and the width a mock name without a width part means
//! (visual-review design §2.2, §2.3).

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;

use crate::tag_instructions::repository_path;

/// The dotted config path of the visual-review table.
pub const VISUAL_REVIEW_KEY: &str = "visual_review";
/// Width for a mock name with no width part, when unset.
pub const DEFAULT_VISUAL_REVIEW_WIDTH: u32 = 1440;

/// Admitted `[visual_review]` settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualReviewSettings {
    /// Repository-relative view manifests, read at admission.
    pub manifests: Vec<String>,
    /// `visual_review.default_width` (default 1440).
    pub default_width: u32,
}

impl Default for VisualReviewSettings {
    fn default() -> Self {
        Self {
            manifests: Vec::new(),
            default_width: DEFAULT_VISUAL_REVIEW_WIDTH,
        }
    }
}

/// Admit `[visual_review]` from one (possibly merged) document.
pub(crate) fn visual_review_from_document(
    document: &toml::Value,
    config_path: &Path,
) -> Result<VisualReviewSettings, OrbitError> {
    let invalid = |detail: String| {
        OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': {detail}",
            redact_home_dir(&config_path.display().to_string())
        ))
    };
    let Some(table) = document.get("visual_review") else {
        return Ok(VisualReviewSettings::default());
    };
    let Some(table) = table.as_table() else {
        return Err(invalid(format!(
            "'{VISUAL_REVIEW_KEY}' must be a table with `manifests` and `default_width`"
        )));
    };
    let mut settings = VisualReviewSettings::default();
    if let Some(manifests) = table.get("manifests") {
        let Some(entries) = manifests.as_array() else {
            return Err(invalid(format!(
                "'{VISUAL_REVIEW_KEY}.manifests' must be an array of repository-relative paths"
            )));
        };
        for entry in entries {
            let Some(path) = entry.as_str() else {
                return Err(invalid(format!(
                    "'{VISUAL_REVIEW_KEY}.manifests' entries must be strings"
                )));
            };
            settings.manifests.push(
                repository_path(path)
                    .map_err(|why| invalid(format!("'{VISUAL_REVIEW_KEY}.manifests' {why}")))?,
            );
        }
    }
    if let Some(width) = table.get("default_width") {
        let Some(width) = width.as_integer().filter(|width| *width > 0) else {
            return Err(invalid(format!(
                "'{VISUAL_REVIEW_KEY}.default_width' must be a positive integer"
            )));
        };
        settings.default_width = u32::try_from(width)
            .map_err(|_| invalid(format!("'{VISUAL_REVIEW_KEY}.default_width' is too large")))?;
    }
    Ok(settings)
}
