//! Shared footprint and block facts, including their persisted representation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub(in crate::application) use crate::application::task::{Anchor, BlockCause};

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
