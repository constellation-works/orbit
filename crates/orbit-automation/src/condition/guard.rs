//! The `when:` guard verdict beside a typed trigger kind (§11). The guard is
//! level-checked at each due moment and has no edge state: the kind supplies
//! the identity (a slot, a batch or a member). Unknown fails closed.

use super::evaluate::Truth;

/// Reason code for a due fire a false guard holds.
pub const GUARD_FALSE: &str = "guard_false";
/// Reason code for a due fire an unknown guard holds.
pub const GUARD_UNKNOWN: &str = "guard_unknown";

/// Whether a due fire may proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardVerdict {
    /// No guard, or the guard is true.
    Open,
    /// The guard is false or unknown: nothing fires.
    Closed {
        /// [`GUARD_FALSE`] or [`GUARD_UNKNOWN`].
        reason: &'static str,
        /// What the guard read, or why it could not be evaluated.
        detail: String,
    },
}

impl GuardVerdict {
    /// The verdict for an evaluated guard expression.
    pub fn from_truth(source: &str, truth: Truth) -> Self {
        match truth {
            Truth::True => Self::Open,
            Truth::False => Self::Closed {
                reason: GUARD_FALSE,
                detail: format!("`{source}` is false"),
            },
            Truth::Unknown(why) => Self::unknown(why),
        }
    }

    /// A guard that could not be evaluated.
    pub fn unknown(why: impl Into<String>) -> Self {
        Self::Closed {
            reason: GUARD_UNKNOWN,
            detail: why.into(),
        }
    }

    /// The report text: `guard_false: <detail>`.
    pub fn describe(&self) -> Option<String> {
        match self {
            Self::Open => None,
            Self::Closed { reason, detail } => Some(format!("{reason}: {detail}")),
        }
    }
}
