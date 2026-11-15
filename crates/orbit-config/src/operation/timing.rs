use std::path::Path;

use orbit_common::OrbitError;

use super::keys::REVIEW_TIMING_KEY;

/// `review.timing`: which review layer, if any, runs before landing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewTimingSetting {
    /// No review before landing (the default, recommended).
    Off,
    /// Hold PR creation for a reviewer that fixes what it finds.
    ///
    /// Deprecated but fully honoured: keep for existing before-PR runs until
    /// an intentional `orbit config set review.timing before_landing`.
    /// New setups use `off` or `before_landing`.
    BeforePr,
    /// Review the open pull request before it lands (recommended when a
    /// pre-landing review is wanted).
    BeforeLanding,
}

impl ReviewTimingSetting {
    /// Every value `review.timing` admits, in the order help lists them.
    pub const CHOICES: &'static [&'static str] = &[
        "off",
        "before_pr",
        "before_landing",
        "none",
        "before-pr",
        "before-landing",
        "after_landing",
        "after-landing",
    ];

    /// The persisted spelling, kept readable by the previous release during
    /// a mixed-version deployment. Reading an alias never changes the policy.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::BeforePr => "before_pr",
            Self::BeforeLanding => "before_landing",
        }
    }

    /// The timing a configured spelling names.
    pub fn parse(raw: &str) -> Result<Self, OrbitError> {
        match raw.trim() {
            "off" | "none" | "after_landing" | "after-landing" => Ok(Self::Off),
            "before_pr" | "before-pr" => Ok(Self::BeforePr),
            "before_landing" | "before-landing" => Ok(Self::BeforeLanding),
            other => Err(OrbitError::InvalidInput(format!(
                "{REVIEW_TIMING_KEY} has invalid value '{other}'; expected one of: {}",
                Self::CHOICES.join(", ")
            ))),
        }
    }

    /// Parse a value from a physical config layer, naming it on failure.
    pub(crate) fn parse_in_file(raw: &str, config_path: &Path) -> Result<Self, OrbitError> {
        Self::parse(raw).map_err(|error| match error {
            OrbitError::InvalidInput(message) => OrbitError::InvalidInput(format!(
                "{}: {message}",
                orbit_common::security::redaction::redact_home_dir(
                    &config_path.display().to_string()
                )
            )),
            other => other,
        })
    }

    /// The timing two before-landing switches select. Both on is refused:
    /// there is one review layer before landing.
    pub(crate) fn from_switches(before_pr: bool, before_landing: bool) -> Option<Self> {
        match (before_pr, before_landing) {
            (true, true) => None,
            (true, false) => Some(Self::BeforePr),
            (false, true) => Some(Self::BeforeLanding),
            (false, false) => Some(Self::Off),
        }
    }

    /// The `(before_pr, before_landing)` switches this timing turns on.
    pub fn switches(self) -> (bool, bool) {
        (self == Self::BeforePr, self == Self::BeforeLanding)
    }

    /// Whether this timing is deprecated but still honoured (`before_pr` in
    /// any spelling). Deprecation never changes the resolved behavior.
    pub fn is_deprecated(self) -> bool {
        self == Self::BeforePr
    }
}
