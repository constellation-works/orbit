//! Shared footprint and block facts, including their persisted representation.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use orbit_common::fs::selector::Selector;
use serde::{Deserialize, Serialize};

/// A footprint entry: a workspace-relative path and whether it names a
/// directory (`dir:` selector) rather than a file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(in crate::application) struct Anchor {
    pub(in crate::application) path: String,
    pub(in crate::application) dir: bool,
}

impl Anchor {
    /// Parse a context selector (`file:`, `dir:`, `symbol:`) or a bare
    /// reservation path. Module and command selectors carry no path.
    pub(in crate::application) fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        let selector = if raw.contains(':') && !raw.starts_with('/') {
            Selector::from_str(raw).ok()
        } else {
            None
        };
        let (path, dir) = match selector {
            Some(Selector::Dir { path }) => (path, true),
            Some(Selector::File { path } | Selector::Symbol { path, .. }) => (path, false),
            Some(Selector::Module { .. } | Selector::Command { .. }) => return None,
            None => (raw.to_string(), false),
        };
        let path = path
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_string();
        if path.is_empty() || path == "." {
            return None;
        }
        Some(Self { path, dir })
    }
}

/// Why a task entered `blocked`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::application) enum BlockCause {
    /// An infrastructure failure the run failure classification names; never
    /// counted against the task's files.
    Infra(&'static str),
    /// A run failed or escalated on the work itself: counted against the
    /// task's footprint.
    Run,
    /// The candidate could not be carried onto a base that moved under it:
    /// counted only against the conflicting paths it recorded.
    BaseConflict(Vec<Anchor>),
    /// A status write made by hand, by the named [actor kind](super::block_cause::actor_kind).
    Manual(&'static str),
    /// Not a failure: a review evidence hold or an operator's cancel.
    NotFailure,
    /// Nothing identifies the cause; reported in its own bucket.
    Unclassified,
}

/// [`BlockCause`] as stored: its infrastructure cause or actor kind by name,
/// which must name one this binary lists to be read back.
#[derive(Serialize, Deserialize)]
enum StoredCause {
    Infra(String),
    Run,
    BaseConflict(Vec<Anchor>),
    Manual(String),
    NotFailure,
    Unclassified,
}

impl Serialize for BlockCause {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.clone() {
            Self::Infra(cause) => StoredCause::Infra(cause.to_string()),
            Self::Run => StoredCause::Run,
            Self::BaseConflict(paths) => StoredCause::BaseConflict(paths),
            Self::Manual(actor) => StoredCause::Manual(actor.to_string()),
            Self::NotFailure => StoredCause::NotFailure,
            Self::Unclassified => StoredCause::Unclassified,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BlockCause {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let known = |names: &[&'static str], name: String| {
            names
                .iter()
                .find(|known| **known == name)
                .copied()
                .ok_or_else(|| serde::de::Error::custom(format!("unknown block cause '{name}'")))
        };
        Ok(match StoredCause::deserialize(deserializer)? {
            StoredCause::Infra(cause) => Self::Infra(known(INFRA_CAUSES, cause)?),
            StoredCause::Run => Self::Run,
            StoredCause::BaseConflict(paths) => Self::BaseConflict(paths),
            StoredCause::Manual(actor) => Self::Manual(known(ACTOR_KINDS, actor)?),
            StoredCause::NotFailure => Self::NotFailure,
            StoredCause::Unclassified => Self::Unclassified,
        })
    }
}

/// One entry into `blocked`: when, the history event that wrote it, and its
/// classified cause.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(in crate::application::codebase) struct Block {
    pub(in crate::application::codebase) at: DateTime<Utc>,
    pub(in crate::application::codebase) event: String,
    pub(in crate::application::codebase) cause: BlockCause,
}

/// Infrastructure causes, in the order the dashboard lists them.
pub(in crate::application::codebase) const INFRA_CAUSES: &[&str] = &[
    "auth",
    "sandbox",
    "disk",
    "provider_limit",
    "provider_unavailable",
    "forge_unavailable",
    "baseline_red",
    "environment",
    "owner_route",
    "transient",
    "interrupted",
];

/// Who made a manual block, in the order the dashboard lists them.
pub(in crate::application::codebase) const ACTOR_KINDS: &[&str] =
    &["operator", "agent", "system", "unknown"];
