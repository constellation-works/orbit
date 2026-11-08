//! A task's crew provenance: why its recorded crew is what it is, and
//! therefore whether the next admission keeps it, redraws it from a pool,
//! or treats it as an explicit pin.
//!
//! Serializes transparently to the spellings task records have always
//! stored (`explicit`, `default`, `pool:<tier>`, `purpose:<purpose>`,
//! `type:<type>/<tier>`, `experiment:<id>/<arm>`, `redraw:<reason>`,
//! `backfill:admission`). Any value the enum does not model — an older or
//! foreign writer's — deserializes to [`CrewSource::Other`] and round-trips
//! verbatim instead of failing or defaulting.

use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::TaskComplexity;

/// Why admission ran a task on a crew other than the one recorded.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RedrawReason {
    /// An explicit run crew replaced the recorded crew.
    RunCrew,
    /// A provider failure hold excluded the recorded crew.
    ProviderHold,
    /// A provider limit excluded the recorded crew.
    ProviderLimit,
    /// The run's crew allowlist excluded the recorded crew.
    Allowlist,
    /// Usage pressure re-drew the pool.
    UsagePressure,
    /// The draw needed an image-capable implementer.
    ImageCapable,
    /// A pool draw.
    Pool,
    /// A claimed leaf drew its crew on its executor.
    ClaimedLeaf,
    /// Any other reason string, preserved verbatim.
    Other(String),
}

impl RedrawReason {
    /// The stored spelling of this reason (`redraw:` carries it verbatim).
    pub fn as_str(&self) -> &str {
        match self {
            Self::RunCrew => "run_crew",
            Self::ProviderHold => "provider_hold",
            Self::ProviderLimit => "provider_limit",
            Self::Allowlist => "allowlist",
            Self::UsagePressure => "usage_pressure",
            Self::ImageCapable => "image_capable",
            Self::Pool => "pool",
            Self::ClaimedLeaf => "claimed_leaf",
            Self::Other(value) => value,
        }
    }

    /// The reason a stored code names; unknown codes stay verbatim.
    pub fn parse(value: &str) -> Self {
        match value {
            "run_crew" => Self::RunCrew,
            "provider_hold" => Self::ProviderHold,
            "provider_limit" => Self::ProviderLimit,
            "allowlist" => Self::Allowlist,
            "usage_pressure" => Self::UsagePressure,
            "image_capable" => Self::ImageCapable,
            "pool" => Self::Pool,
            "claimed_leaf" => Self::ClaimedLeaf,
            other => Self::Other(other.to_string()),
        }
    }
}

impl Display for RedrawReason {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A task's crew provenance. See the module docs for the stored spellings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrewSource {
    /// The caller named the crew (`explicit`).
    Explicit,
    /// The workspace default chain supplied the crew (`default`).
    Default,
    /// A complexity pool draw at this tier (`pool:<tier>`).
    Pool(TaskComplexity),
    /// A purpose pool draw (`purpose:<purpose>`).
    Purpose(String),
    /// A type pool draw (`type:<type>/<tier>`).
    Type {
        /// The classified task type whose pool drew.
        task_type: String,
        /// The tier the pool drew at.
        tier: TaskComplexity,
    },
    /// An experiment arm's crew overlay (`experiment:<id>/<arm>`).
    Experiment {
        /// The experiment that chose the crew.
        experiment: String,
        /// The arm whose overlay chose the crew.
        arm: String,
    },
    /// Admission ran another crew than the recorded one (`redraw:<reason>`).
    Redraw(RedrawReason),
    /// The one-time backfill set the crew from the latest delivery
    /// (`backfill:admission`).
    Backfill,
    /// A legacy or foreign value the enum does not model, verbatim.
    Other(String),
}

impl CrewSource {
    /// Parse a stored value. Never fails: a value no variant models
    /// (including a malformed pool or type spelling) becomes
    /// [`CrewSource::Other`] so it round-trips byte-identically.
    pub fn parse(value: &str) -> Self {
        match value {
            "explicit" => return Self::Explicit,
            "default" => return Self::Default,
            "backfill:admission" => return Self::Backfill,
            _ => {}
        }
        if let Some(tier) = value.strip_prefix("pool:") {
            if let Ok(tier) = tier.parse::<TaskComplexity>() {
                return Self::Pool(tier);
            }
            return Self::Other(value.to_string());
        }
        if let Some(purpose) = value.strip_prefix("purpose:") {
            return Self::Purpose(purpose.to_string());
        }
        if let Some(rest) = value.strip_prefix("type:") {
            if let Some((task_type, tier)) = rest.rsplit_once('/')
                && !task_type.is_empty()
                && let Ok(tier) = tier.parse::<TaskComplexity>()
                && tier.is_assessed()
            {
                return Self::Type {
                    task_type: task_type.to_string(),
                    tier,
                };
            }
            return Self::Other(value.to_string());
        }
        if let Some(rest) = value.strip_prefix("experiment:") {
            if let Some((experiment, arm)) = rest.split_once('/')
                && !experiment.is_empty()
                && !arm.is_empty()
            {
                return Self::Experiment {
                    experiment: experiment.to_string(),
                    arm: arm.to_string(),
                };
            }
            return Self::Other(value.to_string());
        }
        if let Some(reason) = value.strip_prefix("redraw:") {
            return Self::Redraw(RedrawReason::parse(reason));
        }
        Self::Other(value.to_string())
    }

    /// Whether this is a system assignment — a purpose pool, a type pool, a
    /// complexity pool, the default, or an admission write-back — rather
    /// than an explicit pin or a legacy value with no assignment evidence.
    pub fn is_automatic(&self) -> bool {
        matches!(
            self,
            Self::Default
                | Self::Pool(_)
                | Self::Purpose(_)
                | Self::Type { .. }
                | Self::Experiment { .. }
                | Self::Redraw(_)
                | Self::Backfill
        )
    }

    /// Whether this records a crew admission wrote back for one delivery
    /// (an experiment overlay, a redraw, or the backfill). Such a crew is
    /// the latest delivery's, not a standing assignment: the next admission
    /// draws the task's pool again instead of keeping it.
    pub fn is_admission_writeback(&self) -> bool {
        matches!(
            self,
            Self::Experiment { .. } | Self::Redraw(_) | Self::Backfill
        )
    }

    /// The type and tier a [`CrewSource::Type`] source names.
    pub fn type_route(&self) -> Option<(&str, TaskComplexity)> {
        match self {
            Self::Type { task_type, tier } => Some((task_type, *tier)),
            _ => None,
        }
    }

    /// The tier a [`CrewSource::Pool`] source names.
    pub fn pool_tier(&self) -> Option<TaskComplexity> {
        match self {
            Self::Pool(tier) => Some(*tier),
            _ => None,
        }
    }

    /// The purpose a [`CrewSource::Purpose`] source names.
    pub fn purpose(&self) -> Option<&str> {
        match self {
            Self::Purpose(purpose) => Some(purpose),
            _ => None,
        }
    }
}

impl Display for CrewSource {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Explicit => f.write_str("explicit"),
            Self::Default => f.write_str("default"),
            Self::Pool(tier) => write!(f, "pool:{tier}"),
            Self::Purpose(purpose) => write!(f, "purpose:{purpose}"),
            Self::Type { task_type, tier } => write!(f, "type:{task_type}/{tier}"),
            Self::Experiment { experiment, arm } => write!(f, "experiment:{experiment}/{arm}"),
            Self::Redraw(reason) => write!(f, "redraw:{reason}"),
            Self::Backfill => f.write_str("backfill:admission"),
            Self::Other(value) => f.write_str(value),
        }
    }
}

impl FromStr for CrewSource {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse(value))
    }
}

impl From<&str> for CrewSource {
    fn from(value: &str) -> Self {
        Self::parse(value)
    }
}

impl From<String> for CrewSource {
    fn from(value: String) -> Self {
        Self::parse(&value)
    }
}

impl Serialize for CrewSource {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for CrewSource {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::parse(&value))
    }
}
