//! Release version parsing and ordering.
//!
//! Orbit publishes `vMAJOR.MINOR.PATCH` tags, optionally with a `-pre` suffix.
//! `orbit update` needs exactly two answers about them — is this string a
//! version, and is it older than the one running — so this is a small
//! purpose-built type rather than a semver dependency.

use std::cmp::Ordering;
use std::fmt::{Display, Formatter};

use orbit_common::OrbitError;

/// A published Orbit release version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseVersion {
    major: u64,
    minor: u64,
    patch: u64,
    /// Pre-release identifier without the leading `-`, e.g. `rc.1`.
    pre: Option<String>,
}

impl ReleaseVersion {
    /// Parse `0.18.0`, `v0.18.0`, or `0.19.0-rc.1`.
    pub fn parse(value: &str) -> Result<Self, OrbitError> {
        let trimmed = value.trim();
        let body = trimmed.strip_prefix('v').unwrap_or(trimmed);
        let (core, pre) = match body.split_once('-') {
            Some((core, pre)) if !pre.is_empty() => (core, Some(pre.to_string())),
            Some(_) | None => (body, None),
        };
        let mut fields = core.split('.');
        let mut next = |label: &str| -> Result<u64, OrbitError> {
            fields
                .next()
                .filter(|field| !field.is_empty())
                .and_then(|field| field.parse::<u64>().ok())
                .ok_or_else(|| Self::invalid(value, label))
        };
        let major = next("major")?;
        let minor = next("minor")?;
        let patch = next("patch")?;
        if fields.next().is_some() {
            return Err(Self::invalid(value, "trailing component"));
        }
        Ok(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    fn invalid(value: &str, detail: &str) -> OrbitError {
        OrbitError::InvalidInput(format!(
            "'{value}' is not a released Orbit version ({detail}); expected MAJOR.MINOR.PATCH, e.g. 0.18.0"
        ))
    }

    /// The `vMAJOR.MINOR.PATCH` release tag for this version.
    pub fn tag(&self) -> String {
        format!("v{self}")
    }
}

impl Display for ReleaseVersion {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)?;
        match &self.pre {
            Some(pre) => write!(formatter, "-{pre}"),
            None => Ok(()),
        }
    }
}

impl Ord for ReleaseVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            // A pre-release sorts before the release it leads to, and two
            // pre-releases of the same core version compare lexically.
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(left), Some(right)) => left.cmp(right),
            })
    }
}

impl PartialOrd for ReleaseVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
