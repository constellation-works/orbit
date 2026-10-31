//! `[visual_review]`: the view manifests mock names are validated
//! against, the width a mock name without a width part means
//! (visual-review design §2.2, §2.3), the delivery capture's mode,
//! mock budget and `require_established` hold (§4, §6.1), and in `live`
//! the severities that block and are filed (`block_at`, `file_at`, §11.3).

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;

use orbit_types::task::{
    DEFAULT_VISUAL_BLOCK_AT, DEFAULT_VISUAL_FILE_AT, DEFAULT_VISUAL_MAX_FINDINGS,
    DEFAULT_VISUAL_MAX_IMAGE_TOKENS, DEFAULT_VISUAL_MAX_IMAGES, DEFAULT_VISUAL_MAX_MOCKS,
    VisualReviewMode, VisualThreshold,
};
use orbit_types::workflow::SelectiveBeforeLandingMode;

use crate::operation::{OperationPolicy, ReviewTimingSetting};

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
    /// `visual_review.mode` (default `off`): whether delivery captures.
    pub mode: VisualReviewMode,
    /// `visual_review.max_mocks` (default 12): mocks captured per delivery.
    pub max_mocks: u32,
    /// `visual_review.reviewer` (default true): run the comparison
    /// reviewer and its verifier; `false` keeps capture and checks only.
    pub reviewer: bool,
    /// `visual_review.max_images` (default 32): images sent to one
    /// review invocation, canary included.
    pub max_images: u32,
    /// `visual_review.max_image_tokens` (default 48000): estimated
    /// image tokens for one review invocation.
    pub max_image_tokens: u64,
    /// `visual_review.max_findings` (default 15): findings accepted
    /// from one review; extras are dropped with a count.
    pub max_findings: u32,
    /// `visual_review.require_established` (default false): hold a
    /// delivery whose capture is not established.
    pub require_established: bool,
    /// `visual_review.block_at` (default `high`): the lowest confirmed
    /// severity that must be resolved before landing, or `never`.
    pub block_at: VisualThreshold,
    /// `visual_review.file_at` (default `low`): the lowest confirmed
    /// severity filed as a follow-up when not fixed. Never above
    /// `block_at`.
    pub file_at: VisualThreshold,
}

impl Default for VisualReviewSettings {
    fn default() -> Self {
        Self {
            manifests: Vec::new(),
            default_width: DEFAULT_VISUAL_REVIEW_WIDTH,
            mode: VisualReviewMode::Off,
            max_mocks: DEFAULT_VISUAL_MAX_MOCKS,
            reviewer: true,
            max_images: DEFAULT_VISUAL_MAX_IMAGES,
            max_image_tokens: DEFAULT_VISUAL_MAX_IMAGE_TOKENS,
            max_findings: DEFAULT_VISUAL_MAX_FINDINGS,
            require_established: false,
            block_at: DEFAULT_VISUAL_BLOCK_AT,
            file_at: DEFAULT_VISUAL_FILE_AT,
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
            "'{VISUAL_REVIEW_KEY}' must be a table with `mode`, `manifests`, `default_width`, \
             `max_mocks`, `require_established`, `block_at` and `file_at`"
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
    if let Some(mode) = table.get("mode") {
        settings.mode = mode
            .as_str()
            .and_then(VisualReviewMode::parse)
            .ok_or_else(|| {
                invalid(format!(
                    "'{VISUAL_REVIEW_KEY}.mode' must be \"off\", \"shadow\" or \"live\""
                ))
            })?;
    }
    if let Some(max_mocks) = table.get("max_mocks") {
        let Some(max_mocks) = max_mocks
            .as_integer()
            .filter(|max_mocks| (1..=100).contains(max_mocks))
        else {
            return Err(invalid(format!(
                "'{VISUAL_REVIEW_KEY}.max_mocks' must be an integer from 1 to 100"
            )));
        };
        settings.max_mocks = u32::try_from(max_mocks)
            .map_err(|_| invalid(format!("'{VISUAL_REVIEW_KEY}.max_mocks' is too large")))?;
    }
    if let Some(reviewer) = table.get("reviewer") {
        settings.reviewer = reviewer
            .as_bool()
            .ok_or_else(|| invalid(format!("'{VISUAL_REVIEW_KEY}.reviewer' must be a boolean")))?;
    }
    if let Some(max_images) = table.get("max_images") {
        let Some(max_images) = max_images
            .as_integer()
            .filter(|max_images| (2..=200).contains(max_images))
        else {
            return Err(invalid(format!(
                "'{VISUAL_REVIEW_KEY}.max_images' must be an integer from 2 to 200 \
                 (the canary plus at least one image)"
            )));
        };
        settings.max_images = u32::try_from(max_images)
            .map_err(|_| invalid(format!("'{VISUAL_REVIEW_KEY}.max_images' is too large")))?;
    }
    if let Some(max_tokens) = table.get("max_image_tokens") {
        let Some(max_tokens) = max_tokens
            .as_integer()
            .filter(|max_tokens| (1..=1_000_000).contains(max_tokens))
        else {
            return Err(invalid(format!(
                "'{VISUAL_REVIEW_KEY}.max_image_tokens' must be an integer from 1 to 1000000"
            )));
        };
        settings.max_image_tokens = u64::try_from(max_tokens).map_err(|_| {
            invalid(format!(
                "'{VISUAL_REVIEW_KEY}.max_image_tokens' is too large"
            ))
        })?;
    }
    if let Some(max_findings) = table.get("max_findings") {
        let Some(max_findings) = max_findings
            .as_integer()
            .filter(|max_findings| (1..=100).contains(max_findings))
        else {
            return Err(invalid(format!(
                "'{VISUAL_REVIEW_KEY}.max_findings' must be an integer from 1 to 100"
            )));
        };
        settings.max_findings = u32::try_from(max_findings)
            .map_err(|_| invalid(format!("'{VISUAL_REVIEW_KEY}.max_findings' is too large")))?;
    }
    if let Some(require) = table.get("require_established") {
        settings.require_established = require.as_bool().ok_or_else(|| {
            invalid(format!(
                "'{VISUAL_REVIEW_KEY}.require_established' must be a boolean"
            ))
        })?;
    }
    if let Some(block_at) = table.get("block_at") {
        settings.block_at = block_at
            .as_str()
            .and_then(VisualThreshold::parse)
            .filter(|threshold| *threshold != VisualThreshold::Nit)
            .ok_or_else(|| {
                invalid(format!(
                    "'{VISUAL_REVIEW_KEY}.block_at' must be \"critical\", \"high\", \"medium\", \
                     \"low\" or \"never\""
                ))
            })?;
    }
    if let Some(file_at) = table.get("file_at") {
        settings.file_at = file_at
            .as_str()
            .and_then(VisualThreshold::parse)
            .ok_or_else(|| {
                invalid(format!(
                    "'{VISUAL_REVIEW_KEY}.file_at' must be \"critical\", \"high\", \"medium\", \
                     \"low\", \"nit\" or \"never\""
                ))
            })?;
    }
    if settings.file_at.rank() > settings.block_at.rank() {
        return Err(invalid(format!(
            "'{VISUAL_REVIEW_KEY}.file_at' (\"{}\") must not be above \
             '{VISUAL_REVIEW_KEY}.block_at' (\"{}\"): a finding that blocks must also be filed \
             when it lands unresolved",
            settings.file_at.as_str(),
            settings.block_at.as_str()
        )));
    }
    Ok(settings)
}

/// Whether `operation` runs a review before landing that can resolve a
/// blocking visual finding: `review.timing` not off, or selective
/// before-landing review enforced.
#[must_use]
pub fn visual_review_layer(operation: &OperationPolicy) -> bool {
    operation.review_timing() != ReviewTimingSetting::Off
        || operation.selective_before_landing.mode.value == SelectiveBeforeLandingMode::Enforce
}

/// Warn when `visual_review.mode = "live"` has no review layer (§1): with
/// nothing to resolve a blocking finding before landing, blocking findings
/// are filed as `high` follow-ups and nothing blocks.
pub(crate) fn warn_live_without_review_layer(
    settings: &VisualReviewSettings,
    operation: &OperationPolicy,
    config_path: &Path,
) {
    if settings.mode == VisualReviewMode::Live
        && settings.block_at != VisualThreshold::Never
        && !visual_review_layer(operation)
    {
        tracing::warn!(
            config = %redact_home_dir(&config_path.display().to_string()),
            key = "visual_review.mode",
            "visual_review.mode = \"live\" without a review before landing (review.timing is off) \
             blocks nothing: confirmed findings at or above visual_review.block_at are filed as \
             high follow-ups instead"
        );
    }
}
