//! `[workflow.review_skip_tags.<tag>]`: a workspace-mapped tag whose
//! candidates skip agent review when every changed path matches the
//! tag's `paths` and its optional `guard` passes at the base revision.
//!
//! The tables layer like tag instructions: the merged document is
//! admitted once, and workspace entries replace global ones per tag.
//! Only the mapping lives here; the host reads the guard at the
//! delivery's base revision, so a delivery cannot weaken its own guard.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::path_filter::PathFilter;
use orbit_common::security::redaction::redact_home_dir;
use orbit_types::task::{LIGHT_DELIVERY_TAGS, MAX_REVIEW_SKIP_TAGS, ReviewSkipProfile};
use orbit_types::workflow::normalize_instruction_tag;

use crate::tag_instructions::repository_path;

/// The dotted config path of the review-skip table.
pub const REVIEW_SKIP_TAGS_KEY: &str = "workflow.review_skip_tags";

/// The admitted review-skip table, keyed by normalized tag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewSkipSettings {
    tags: BTreeMap<String, ReviewSkipProfile>,
}

impl ReviewSkipSettings {
    /// The profile `tag` (already normalized) maps, if any.
    pub fn profile(&self, tag: &str) -> Option<&ReviewSkipProfile> {
        self.tags.get(tag)
    }

    /// Every configured (tag, profile), in tag order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &ReviewSkipProfile)> {
        self.tags
            .iter()
            .map(|(tag, profile)| (tag.as_str(), profile))
    }

    /// Every configured tag, in order.
    pub fn tags(&self) -> impl Iterator<Item = &str> {
        self.tags.keys().map(String::as_str)
    }

    /// Whether `tag` (normalized form compared case-insensitively by
    /// callers that normalize first) is a mapped review-skip tag.
    pub fn contains(&self, tag: &str) -> bool {
        self.tags.contains_key(tag)
    }

    /// Whether no tag is mapped.
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    /// The profiles as a map, for capture in a claim's validation gates.
    pub fn as_map(&self) -> BTreeMap<String, ReviewSkipProfile> {
        self.tags.clone()
    }
}

/// Admit `workflow.review_skip_tags` from one (possibly merged) document.
pub(crate) fn review_skip_tags_from_document(
    document: &toml::Value,
    config_path: &Path,
) -> Result<ReviewSkipSettings, OrbitError> {
    let Some(value) = document
        .get("workflow")
        .and_then(|workflow| workflow.get("review_skip_tags"))
    else {
        return Ok(ReviewSkipSettings::default());
    };
    let invalid = |detail: String| {
        OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': {detail}",
            redact_home_dir(&config_path.display().to_string())
        ))
    };
    let Some(table) = value.as_table() else {
        return Err(invalid(format!(
            "'{REVIEW_SKIP_TAGS_KEY}' must be a table of `[{REVIEW_SKIP_TAGS_KEY}.<tag>]` tables"
        )));
    };
    if table.len() > MAX_REVIEW_SKIP_TAGS {
        return Err(invalid(format!(
            "'{REVIEW_SKIP_TAGS_KEY}' maps {} tags; at most {MAX_REVIEW_SKIP_TAGS} are allowed",
            table.len()
        )));
    }
    let mut tags = BTreeMap::new();
    for (raw_tag, profile) in table {
        let key = format!("{REVIEW_SKIP_TAGS_KEY}.{raw_tag}");
        let Some(tag) = normalize_instruction_tag(raw_tag) else {
            return Err(invalid(format!("'{key}' names a blank tag")));
        };
        if LIGHT_DELIVERY_TAGS.contains(&tag.as_str()) {
            return Err(invalid(format!(
                "'{key}' reuses the built-in tag '{tag}'; built-in light-delivery tags \
                 keep `workflow.light_delivery_paths` and cannot be remapped"
            )));
        }
        let Some(profile) = profile.as_table() else {
            return Err(invalid(format!(
                "'{key}' must be a table with `paths` and an optional `guard`"
            )));
        };
        for field in profile.keys() {
            if field != "paths" && field != "guard" {
                return Err(invalid(format!(
                    "'{key}.{field}' is not a review-skip setting; use `paths` or `guard`"
                )));
            }
        }
        let paths_key = format!("{key}.paths");
        let Some(paths_value) = profile.get("paths") else {
            return Err(invalid(format!(
                "'{paths_key}' is required: a non-empty array of path globs"
            )));
        };
        let Some(raw_paths) = paths_value.as_array() else {
            return Err(invalid(format!(
                "'{paths_key}' must be an array of path glob strings"
            )));
        };
        if raw_paths.is_empty() {
            return Err(invalid(format!(
                "'{paths_key}' is empty; name at least one path glob (use `[\"**\"]` for every path)"
            )));
        }
        let mut paths = Vec::with_capacity(raw_paths.len());
        for entry in raw_paths {
            let Some(entry) = entry.as_str() else {
                return Err(invalid(format!(
                    "'{paths_key}' must be an array of path glob strings"
                )));
            };
            if entry.trim().is_empty() {
                return Err(invalid(format!(
                    "'{paths_key}' contains an empty path glob"
                )));
            }
            paths.push(entry.to_string());
        }
        PathFilter::new(&paths_key, &paths).map_err(|error| invalid(error.to_string()))?;
        let guard = match profile.get("guard") {
            None => None,
            Some(toml::Value::String(path)) => {
                Some(repository_path(path).map_err(|why| invalid(format!("'{key}.guard' {why}")))?)
            }
            Some(_) => {
                return Err(invalid(format!(
                    "'{key}.guard' must be a repository-relative executable path"
                )));
            }
        };
        if tags
            .insert(tag.clone(), ReviewSkipProfile { paths, guard })
            .is_some()
        {
            return Err(invalid(format!(
                "'{REVIEW_SKIP_TAGS_KEY}' maps tag '{tag}' more than once; tags match \
                 case-insensitively"
            )));
        }
    }
    Ok(ReviewSkipSettings { tags })
}
