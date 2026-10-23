//! Per-drain forecast controller mode.

use serde::{Deserialize, Serialize};

/// The forecast controller policy captured when a local drain is submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoConcurrencyMode {
    /// Admit without forecasting, retaining all other safety gates.
    Off,
    /// Record forecasts and hypothetical decisions without enforcing them.
    Shadow,
    /// Enforce the existing forecast fit controller.
    Enforce,
}

impl AutoConcurrencyMode {
    /// The canonical CLI, tool and persisted spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::Enforce => "enforce",
        }
    }
}

impl std::str::FromStr for AutoConcurrencyMode {
    type Err = &'static str;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "off" => Ok(Self::Off),
            "shadow" => Ok(Self::Shadow),
            "enforce" => Ok(Self::Enforce),
            _ => Err("auto_concurrency must be off, shadow or enforce"),
        }
    }
}
