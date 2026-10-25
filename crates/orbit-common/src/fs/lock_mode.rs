//! Per-path footprint lock modes: which selector overlaps block admission.
//!
//! Every footprint-overlap check that admits or defers work asks
//! [`FootprintLockModes::blocks`]. Two selectors that overlap block each
//! other unless their overlap point (the narrower anchor, or the common one
//! when the anchors are equal, see [`OverlapScope::overlap_point`]) is a path
//! whose first matching entry is `shared`. Anchor-less selectors (`module:` /
//! `command:`) have no overlap point and always block. With no entries every
//! overlap blocks, which is the all-exclusive behaviour.
//!
//! A shared path trades prevention for repair: two tasks may both change it,
//! and a conflict between them is resolved when the second one lands.

use std::sync::Arc;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::selector::{OverlapScope, overlaps};

/// How an overlap on one path is treated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FootprintLockMode {
    /// An overlap on the path blocks admission.
    #[default]
    Exclusive,
    /// An overlap on the path is waived; a conflict is resolved at landing.
    Shared,
}

impl FootprintLockMode {
    /// Every mode, as configured, in the order errors name them.
    pub const CHOICES: [&'static str; 2] = ["exclusive", "shared"];

    /// A mode by its configured name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "exclusive" => Some(Self::Exclusive),
            "shared" => Some(Self::Shared),
            _ => None,
        }
    }
}

/// One admitted lock-mode entry: a regex over the normalized anchor path and
/// the mode a matching path takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FootprintLockModeEntry {
    /// Rust `regex` searched for in the anchor path; anchor it with `^`/`$`.
    pub pattern: String,
    /// The mode of a path this entry is the first to match.
    pub mode: FootprintLockMode,
}

/// The ordered lock-mode entries, compiled once.
///
/// Cheap to clone. The default has no entries, so every overlap blocks.
#[derive(Debug, Clone, Default)]
pub struct FootprintLockModes {
    rules: Arc<[(Regex, FootprintLockMode)]>,
}

impl FootprintLockModes {
    /// Compile `entries` in order. The error names the first entry whose
    /// pattern does not compile, by its 1-based position and pattern.
    pub fn new(entries: &[FootprintLockModeEntry]) -> Result<Self, String> {
        let rules = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                Regex::new(&entry.pattern)
                    .map(|regex| (regex, entry.mode))
                    .map_err(|error| {
                        format!(
                            "entry {} (pattern `{}`) is not a valid regex: {error}",
                            index + 1,
                            entry.pattern
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            rules: rules.into(),
        })
    }

    /// Whether no entry is configured, so every overlap blocks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The mode of `path`: the first matching entry's, else exclusive.
    #[must_use]
    pub fn mode_of(&self, path: &str) -> FootprintLockMode {
        self.rules
            .iter()
            .find(|(regex, _)| regex.is_match(path))
            .map_or(FootprintLockMode::Exclusive, |(_, mode)| *mode)
    }

    /// Whether `left` and `right` overlap at a point that is not shared.
    #[must_use]
    pub fn blocks(&self, left: &OverlapScope, right: &OverlapScope) -> bool {
        if !left.overlaps(right) {
            return false;
        }
        if self.is_empty() {
            return true;
        }
        left.overlap_point(right)
            .is_none_or(|point| self.mode_of(point) == FootprintLockMode::Exclusive)
    }

    /// [`Self::blocks`] on two selector strings. A selector that does not
    /// parse overlaps nothing, as with [`overlaps`].
    #[must_use]
    pub fn selectors_block(&self, left: &str, right: &str) -> bool {
        if self.is_empty() {
            return overlaps(left, right);
        }
        match (OverlapScope::parse(left), OverlapScope::parse(right)) {
            (Some(left), Some(right)) => self.blocks(&left, &right),
            _ => false,
        }
    }

    /// Whether any selector of `left` blocks any selector of `right`.
    #[must_use]
    pub fn footprints_block(&self, left: &[String], right: &[String]) -> bool {
        left.iter()
            .any(|a| right.iter().any(|b| self.selectors_block(a, b)))
    }
}
