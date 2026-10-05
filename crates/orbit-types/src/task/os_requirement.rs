//! The `os:` tag namespace: which host operating system a task needs.
//!
//! A task routes itself to a host by carrying `os:linux`, `os:macos` or
//! `os:windows`. No `os:` tag means any host; several mean any one of them.
//! The namespace is reserved: every task-tag write rejects any other `os:*`
//! value, so a typo can never quietly become "runs anywhere".
//!
//! [`TaskOsRequirement`] is the one parse of those tags. Local admission and
//! pull admission read it rather than raw strings, so the drain, ship
//! discovery, `orbit run ship` and the owner handing out claims cannot
//! disagree about which host a task waits for.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::task::TaskError;

/// The reserved tag prefix.
pub const OS_TAG_PREFIX: &str = "os:";

/// A host operating system a task can require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostOs {
    Linux,
    Macos,
    Windows,
}

impl HostOs {
    /// Every accepted value, in tag order.
    pub const ALL: [Self; 3] = [Self::Linux, Self::Macos, Self::Windows];

    /// The value after `os:`, and the spelling `std::env::consts::OS` uses.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Windows => "windows",
        }
    }

    /// Parse an OS name, ignoring case and surrounding whitespace.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        Self::ALL
            .into_iter()
            .find(|os| os.as_str().eq_ignore_ascii_case(value))
    }

    /// The OS this binary runs on, or `None` on one outside the namespace.
    #[must_use]
    pub fn current() -> Option<Self> {
        Self::parse(std::env::consts::OS)
    }

    /// The tag that requires this OS.
    #[must_use]
    pub fn tag(self) -> String {
        format!("{OS_TAG_PREFIX}{}", self.as_str())
    }
}

impl fmt::Display for HostOs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The host operating systems a task's `os:` tags admit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TaskOsRequirement {
    /// Any one of these satisfies the task. Empty with no `invalid` tag:
    /// any host.
    pub any_of: BTreeSet<HostOs>,
    /// Stored `os:*` tags outside the namespace. Writes reject them, so only a
    /// task stored before the namespace was reserved can carry one; no host
    /// satisfies it until it is retagged.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub invalid: Vec<String>,
}

impl TaskOsRequirement {
    /// Parse the `os:` tags among `tags`. Matching ignores case.
    #[must_use]
    pub fn from_tags(tags: &[String]) -> Self {
        let mut requirement = Self::default();
        for tag in tags {
            let Some(value) = os_tag_value(tag) else {
                continue;
            };
            match HostOs::parse(value) {
                Some(os) => {
                    requirement.any_of.insert(os);
                }
                None => requirement.invalid.push(tag.trim().to_string()),
            }
        }
        requirement
    }

    /// Whether the task carries no `os:` tag at all.
    #[must_use]
    pub fn is_unrestricted(&self) -> bool {
        self.any_of.is_empty() && self.invalid.is_empty()
    }

    /// Whether a host running `host` may start the task. `None` is a host
    /// whose OS is unknown or outside the namespace: it runs only untagged
    /// tasks.
    #[must_use]
    pub fn satisfied_by(&self, host: Option<HostOs>) -> bool {
        self.invalid.is_empty()
            && (self.any_of.is_empty() || host.is_some_and(|host| self.any_of.contains(&host)))
    }

    /// What the task waits for on `host`, or `None` when `host` may start it:
    /// `waits for a macos host (os:macos)`.
    #[must_use]
    pub fn unsatisfied_reason(&self, host: Option<HostOs>) -> Option<String> {
        if self.satisfied_by(host) {
            return None;
        }
        if !self.invalid.is_empty() {
            return Some(format!(
                "carries unsupported OS tag(s) {}; no host can run it until it is retagged with \
                 one of {}",
                quoted(self.invalid.iter().map(String::as_str)),
                accepted_tags()
            ));
        }
        Some(format!("waits for {}", self.describe_hosts()))
    }

    /// `a macos host (os:macos)`, or `a linux or macos host (os:linux,
    /// os:macos)`; `any host` when unrestricted.
    #[must_use]
    pub fn describe_hosts(&self) -> String {
        if self.any_of.is_empty() {
            return "any host".to_string();
        }
        let names = self
            .any_of
            .iter()
            .map(|os| os.as_str())
            .collect::<Vec<_>>()
            .join(" or ");
        let tags = self
            .any_of
            .iter()
            .map(|os| os.tag())
            .collect::<Vec<_>>()
            .join(", ");
        format!("a {names} host ({tags})")
    }
}

/// Reject any `os:*` tag outside the reserved namespace.
///
/// Every task-tag write calls this, so a typo such as `os:mac` fails the write
/// instead of leaving a task that no host would ever admit.
pub fn validate_os_tags(tags: &[String]) -> Result<(), TaskError> {
    let invalid = TaskOsRequirement::from_tags(tags).invalid;
    if invalid.is_empty() {
        return Ok(());
    }
    Err(TaskError::Invalid(format!(
        "unsupported OS tag(s) {}: the `{OS_TAG_PREFIX}` tag namespace is reserved for {}",
        quoted(invalid.iter().map(String::as_str)),
        accepted_tags()
    )))
}

/// The value of an `os:` tag, matching the prefix without regard to case.
fn os_tag_value(tag: &str) -> Option<&str> {
    let tag = tag.trim();
    let prefix = tag.get(..OS_TAG_PREFIX.len())?;
    prefix
        .eq_ignore_ascii_case(OS_TAG_PREFIX)
        .then(|| &tag[OS_TAG_PREFIX.len()..])
}

fn accepted_tags() -> String {
    HostOs::ALL
        .iter()
        .map(|os| format!("`{}`", os.tag()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn quoted<'a>(values: impl Iterator<Item = &'a str>) -> String {
    values
        .map(|value| format!("`{value}`"))
        .collect::<Vec<_>>()
        .join(", ")
}
