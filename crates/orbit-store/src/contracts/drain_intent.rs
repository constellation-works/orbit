//! The per-workspace drain intent (standing drain phase 1): whether an
//! operator last started this workspace's drain or paused it, and who did.

use chrono::{DateTime, Utc};

/// A recorded drain intent. A workspace with no record is `off`, the
/// default: nobody has started or stopped a drain since the intent existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainIntentState {
    /// A drain was last started by hand.
    On,
    /// A drain was last stopped by hand (`orbit run auto --stop` or the
    /// dashboard's Stop).
    Paused,
}

impl DrainIntentState {
    /// The stored and reported token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Paused => "paused",
        }
    }

    /// Parse a stored token; `None` for one this binary does not know.
    #[must_use]
    pub fn parse_stored(token: &str) -> Option<Self> {
        match token {
            "on" => Some(Self::On),
            "paused" => Some(Self::Paused),
            _ => None,
        }
    }
}

/// The workspace's current drain intent and its attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainIntentRecord {
    pub intent: DrainIntentState,
    /// Who set it: the acting identity of the process that started or
    /// stopped the drain.
    pub actor: String,
    /// The surface it came from (`cli`, `dashboard`, `mcp`).
    pub source: String,
    pub reason: Option<String>,
    pub updated_at: DateTime<Utc>,
}
