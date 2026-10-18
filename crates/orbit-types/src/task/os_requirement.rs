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

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::task::TaskError;

/// The reserved tag prefix.
pub const OS_TAG_PREFIX: &str = "os:";

/// A host operating system a task can require.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
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
    /// Crew whose executor `host_os` narrowed `any_of`. Absent when the
    /// requirement is the task's tags alone, so that description stays
    /// `a macos host (os:macos)`.
    #[serde(skip)]
    pub crew: Option<String>,
}

/// A crew's executor `host_os`, carried into admission beside the task's tags.
///
/// Only crews whose executor declares `host_os` appear. Omission is
/// unrestricted and has no entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorOsLimit {
    /// Executor definition that declared the set.
    pub executor: String,
    /// Non-empty set from that definition.
    pub hosts: BTreeSet<HostOs>,
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

    /// Intersect these tags with an executor's declared `host_os`.
    ///
    /// `None` means the executor omits `host_os`: the requirement is returned
    /// unchanged, description included. A declared set keeps only the hosts
    /// in both and names `crew` in the wait. An empty intersection is an
    /// error naming the crew, the executor constraint and the task's tags,
    /// because the task would otherwise be stored where no host can run it.
    ///
    /// Stored tags outside the namespace keep their existing refusal and are
    /// not combined with the executor constraint.
    pub fn intersect_executor(
        &self,
        crew: &str,
        executor: &str,
        host_os: Option<&BTreeSet<HostOs>>,
    ) -> Result<Self, String> {
        let Some(limit) = host_os else {
            return Ok(self.clone());
        };
        if !self.invalid.is_empty() {
            return Ok(self.clone());
        }
        if self.any_of.is_empty() {
            let mut narrowed = self.clone();
            narrowed.any_of.clone_from(limit);
            narrowed.crew = Some(crew.to_string());
            return Ok(narrowed);
        }
        let intersection = self
            .any_of
            .intersection(limit)
            .copied()
            .collect::<BTreeSet<_>>();
        if intersection.is_empty() {
            let constraint = limit
                .iter()
                .map(|os| os.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let tag_list = self.any_of.iter().map(|os| os.tag()).collect::<Vec<_>>();
            return Err(format!(
                "crew `{crew}` (executor `{executor}`, host_os: {constraint}) does not intersect \
                 the task's tags {}; no host can run it",
                quoted(tag_list.iter().map(String::as_str))
            ));
        }
        let mut narrowed = self.clone();
        narrowed.any_of = intersection;
        narrowed.crew = Some(crew.to_string());
        Ok(narrowed)
    }

    /// Tags intersected with the assigned crew's executor limit.
    ///
    /// `limits` is keyed by crew name and holds only crews whose executor
    /// declares `host_os`. No crew, or a crew with no entry, is the tag
    /// requirement alone.
    pub fn for_crew(
        tags: &[String],
        crew: Option<&str>,
        limits: &BTreeMap<String, ExecutorOsLimit>,
    ) -> Result<Self, String> {
        let requirement = Self::from_tags(tags);
        let Some(name) = crew.map(str::trim).filter(|name| !name.is_empty()) else {
            return Ok(requirement);
        };
        let Some(limit) = limit_for_crew(name, limits) else {
            return Ok(requirement);
        };
        requirement.intersect_executor(name, &limit.executor, Some(&limit.hosts))
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
        if let Some(crew) = &self.crew {
            return format!("a {names} host (crew {crew})");
        }
        let tags = self
            .any_of
            .iter()
            .map(|os| os.tag())
            .collect::<Vec<_>>()
            .join(", ");
        format!("a {names} host ({tags})")
    }
}

fn limit_for_crew<'a>(
    crew: &str,
    limits: &'a BTreeMap<String, ExecutorOsLimit>,
) -> Option<&'a ExecutorOsLimit> {
    if let Some(limit) = limits.get(crew) {
        return Some(limit);
    }
    limits
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(crew))
        .map(|(_, limit)| limit)
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
