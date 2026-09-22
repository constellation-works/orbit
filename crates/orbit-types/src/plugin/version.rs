//! Minimal semantic-version matching for `spec.requires.orbit`.
//!
//! Orbit does not take a semver dependency for one comparison, so this is the
//! subset plugin manifests need: comparators `>=`, `>`, `<=`, `<`, `=`, `^`,
//! `~`, a bare version (caret), x-ranges, `*` (any), space-separated AND,
//! and `||` OR. Pre-release tags follow SemVer precedence.

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
            .then_with(|| compare_prerelease(&self.pre, &other.pre))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.cmp_release(other)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl FromStr for Version {
    type Err = VersionError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let trimmed = raw.trim().strip_prefix('v').unwrap_or(raw.trim());
        // Build metadata never participates in ordering.
        let trimmed = match trimmed.split_once('+') {
            Some((kept, build)) if valid_identifiers(build, false) => kept,
            Some(_) => return Err(VersionError::InvalidVersion(raw.to_string())),
            None => trimmed,
        };
        let (numeric, pre) = match trimmed.split_once('-') {
            Some((numeric, pre)) if valid_identifiers(pre, true) => (numeric, pre.to_string()),
            Some(_) => return Err(VersionError::InvalidVersion(raw.to_string())),
            None => (trimmed, String::new()),
        };
        let mut parts = numeric.split('.');
        let mut next = || {
            parts
                .next()
                .filter(|part| {
                    !part.is_empty()
                        && part.bytes().all(|byte| byte.is_ascii_digit())
                        && (part.len() == 1 || !part.starts_with('0'))
                })
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

fn valid_identifiers(value: &str, reject_numeric_leading_zeroes: bool) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && (!reject_numeric_leading_zeroes
                    || !identifier.bytes().all(|byte| byte.is_ascii_digit())
                    || identifier.len() == 1
                    || !identifier.starts_with('0'))
        })
}

fn compare_prerelease(left: &str, right: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    match (left.is_empty(), right.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => left
            .split('.')
            .zip(right.split('.'))
            .map(|(left, right)| {
                match (
                    left.bytes().all(|byte| byte.is_ascii_digit()),
                    right.bytes().all(|byte| byte.is_ascii_digit()),
                ) {
                    (true, true) => left.len().cmp(&right.len()).then_with(|| left.cmp(right)),
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    (false, false) => left.cmp(right),
                }
            })
            .find(|ordering| *ordering != Ordering::Equal)
            .unwrap_or_else(|| left.split('.').count().cmp(&right.split('.').count())),
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

    fn prerelease_bound(&self) -> Option<&Version> {
        match self {
            Self::Any => None,
            Self::Exact(version)
            | Self::Greater(version)
            | Self::GreaterEq(version)
            | Self::Less(version)
            | Self::LessEq(version)
            | Self::Caret(version)
            | Self::Tilde(version) => Some(version),
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
                if let Some(wildcard_range) = parse_x_range(token)? {
                    comparators.extend(wildcard_range);
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
                && (version.pre.is_empty()
                    || comparators.iter().any(|comparator| {
                        comparator.prerelease_bound().is_some_and(|bound| {
                            !bound.pre.is_empty()
                                && (bound.major, bound.minor, bound.patch)
                                    == (version.major, version.minor, version.patch)
                        })
                    }))
        })
    }
}

fn parse_x_range(token: &str) -> Result<Option<Vec<Comparator>>, VersionError> {
    let parts = token.split('.').collect::<Vec<_>>();
    let wildcard = |part: &str| part.eq_ignore_ascii_case("x") || part == "*";
    if !parts.iter().any(|part| wildcard(part)) {
        return Ok(None);
    }
    let invalid = || {
        VersionError::InvalidRange(
            token.to_string(),
            "x-ranges must use numeric components followed only by x or *".to_string(),
        )
    };
    if !(1..=3).contains(&parts.len()) {
        return Err(invalid());
    }
    let first_wildcard = parts
        .iter()
        .position(|part| wildcard(part))
        .ok_or_else(invalid)?;
    if parts[first_wildcard..].iter().any(|part| !wildcard(part)) {
        return Err(invalid());
    }
    match first_wildcard {
        0 => Ok(Some(vec![Comparator::Any])),
        1 => {
            let major: u64 = parts[0].parse().map_err(|_| invalid())?;
            let upper = major.checked_add(1).ok_or_else(invalid)?;
            Ok(Some(vec![
                Comparator::GreaterEq(Version::new(major, 0, 0)),
                Comparator::Less(Version::new(upper, 0, 0)),
            ]))
        }
        2 => {
            let major: u64 = parts[0].parse().map_err(|_| invalid())?;
            let minor: u64 = parts[1].parse().map_err(|_| invalid())?;
            Ok(Some(vec![Comparator::Tilde(Version::new(major, minor, 0))]))
        }
        _ => Err(invalid()),
    }
}

impl Display for SemverRange {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.source)
    }
}
