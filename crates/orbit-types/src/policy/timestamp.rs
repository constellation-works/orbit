use std::fmt;
use std::str::FromStr;
use std::time::SystemTime;

use serde::{Deserialize, Deserializer, Serialize};

use crate::timestamp::{UtcTimestamp, deserialize_legacy_utc_timestamp};

use super::PolicyError;

/// A UTC timestamp for persisted policy definitions and resources.
///
/// Uses the legacy UTC wire format shared with other persisted records and
/// retains policy-domain errors for parsing and system-time conversion.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PolicyTimestamp(UtcTimestamp);

impl TryFrom<SystemTime> for PolicyTimestamp {
    type Error = PolicyError;

    fn try_from(value: SystemTime) -> Result<Self, Self::Error> {
        UtcTimestamp::try_from(value).map(Self).map_err(|_| {
            PolicyError::Invalid("system time is outside the policy date range".into())
        })
    }
}

impl FromStr for PolicyTimestamp {
    type Err = PolicyError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        input.parse().map(Self).map_err(|_| {
            PolicyError::Invalid(format!("invalid RFC 3339 policy timestamp {input:?}"))
        })
    }
}

impl fmt::Debug for PolicyTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for PolicyTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl<'de> Deserialize<'de> for PolicyTimestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_legacy_utc_timestamp(deserializer)
    }
}
