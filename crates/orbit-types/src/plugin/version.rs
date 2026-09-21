//! Minimal semantic-version matching for `spec.requires.orbit`.
//!
//! Orbit does not take a semver dependency for one comparison, so this is the
//! subset plugin manifests need: comparators `>=`, `>`, `<=`, `<`, `=`, `^`,
//! `~`, a bare version (caret), `*` (any), space-separated AND, and `||` OR.
//! Pre-release tags are compared lexically after the numeric triple.

use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Protocol major the host speaks. A manifest whose `requires.host_api`
/// differs is registered inactive with a diagnostic rather than loaded.
pub const PLUGIN_HOST_API: u32 = 1;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum VersionError {
    #[error("invalid version '{0}': expected MAJOR.MINOR.PATCH")]
    InvalidVersion(String),
    #[error("invalid version range '{0}': {1}")]
    InvalidRange(String, String),
}

/// A `MAJOR.MINOR.PATCH[-pre]` version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Empty for a release; a pre-release sorts before its release.
    pub pre: String,
}

impl Version {
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
            pre: String::new(),
        }
    }

    fn cmp_release(&self, other: &Self) -> std::cmp::Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => std::cmp::Ordering::Equal,
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl FromStr for Version {
    type Err = VersionError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let trimmed = raw.trim().trim_start_matches('v');
        // Build metadata never participates in ordering.
        let trimmed = trimmed.split_once('+').map_or(trimmed, |(kept, _)| kept);
        let (numeric, pre) = match trimmed.split_once('-') {
            Some((numeric, pre)) => (numeric, pre.to_string()),
            None => (trimmed, String::new()),
        };
        let mut parts = numeric.split('.');
        let mut next = || {
            parts
                .next()
                .and_then(|part| part.parse::<u64>().ok())
                .ok_or_else(|| VersionError::InvalidVersion(raw.to_string()))
        };
        let major = next()?;
        let minor = next()?;
        let patch = next()?;
        if parts.next().is_some() {
            return Err(VersionError::InvalidVersion(raw.to_string()));
        }
        Ok(Self {
            major,
            minor,
            patch,
            pre,
        })
    }
}

impl TryFrom<String> for Version {
    type Error = VersionError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Version> for String {
    fn from(value: Version) -> Self {
        value.to_string()
    }
}

impl Display for Version {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Comparator {
    Any,
    Exact(Version),
    Greater(Version),
    GreaterEq(Version),
    Less(Version),
    LessEq(Version),
    Caret(Version),
    Tilde(Version),
}

impl Comparator {
    fn matches(&self, version: &Version) -> bool {
        use std::cmp::Ordering::*;
        match self {
            Self::Any => true,
            Self::Exact(bound) => version.cmp_release(bound) == Equal,
            Self::Greater(bound) => version.cmp_release(bound) == Greater,
            Self::GreaterEq(bound) => version.cmp_release(bound) != Less,
            Self::Less(bound) => version.cmp_release(bound) == Less,
            Self::LessEq(bound) => version.cmp_release(bound) != Greater,
            Self::Caret(bound) => {
                if version.cmp_release(bound) == Less {
                    return false;
                }
                if bound.major > 0 {
                    version.major == bound.major
                } else if bound.minor > 0 {
                    version.major == 0 && version.minor == bound.minor
                } else {
                    version.major == 0 && version.minor == 0 && version.patch == bound.patch
                }
            }
            Self::Tilde(bound) => {
                version.cmp_release(bound) != Less
                    && version.major == bound.major
                    && version.minor == bound.minor
            }
        }
    }
}

/// A version range: alternatives separated by `||`, each an AND of
/// comparators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemverRange {
    source: String,
    alternatives: Vec<Vec<Comparator>>,
}

impl SemverRange {
    pub fn parse(raw: &str) -> Result<Self, VersionError> {
        let invalid =
            |reason: &str| VersionError::InvalidRange(raw.to_string(), reason.to_string());
        let mut alternatives = Vec::new();
        for alternative in raw.split("||") {
            let mut comparators = Vec::new();
            for token in alternative.split(|c: char| c.is_whitespace() || c == ',') {
                let token = token.trim();
                if token.is_empty() {
                    continue;
                }
                let comparator = if token == "*" {
                    Comparator::Any
                } else if let Some(rest) = token.strip_prefix(">=") {
                    Comparator::GreaterEq(rest.parse()?)
                } else if let Some(rest) = token.strip_prefix("<=") {
                    Comparator::LessEq(rest.parse()?)
                } else if let Some(rest) = token.strip_prefix('>') {
                    Comparator::Greater(rest.parse()?)
                } else if let Some(rest) = token.strip_prefix('<') {
                    Comparator::Less(rest.parse()?)
                } else if let Some(rest) = token.strip_prefix('=') {
                    Comparator::Exact(rest.parse()?)
                } else if let Some(rest) = token.strip_prefix('^') {
                    Comparator::Caret(rest.parse()?)
                } else if let Some(rest) = token.strip_prefix('~') {
                    Comparator::Tilde(rest.parse()?)
                } else {
                    Comparator::Caret(token.parse()?)
                };
                comparators.push(comparator);
            }
            if comparators.is_empty() {
                return Err(invalid("empty comparator set"));
            }
            alternatives.push(comparators);
        }
        Ok(Self {
            source: raw.trim().to_string(),
            alternatives,
        })
    }

    pub fn matches(&self, version: &Version) -> bool {
        self.alternatives.iter().any(|comparators| {
            comparators
                .iter()
                .all(|comparator| comparator.matches(version))
        })
    }
}

impl Display for SemverRange {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.source)
    }
}
