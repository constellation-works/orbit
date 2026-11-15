//! The schedule readiness a routine or auto-task row shows. `routines` and
//! `auto_tasks` both project it and `routines` sits above `auto_tasks`, so the
//! state sits below both [ORB-22623].

use serde_json::{Value, json};

/// Operator-facing schedule readiness. Theoretical next-slot math may still be
/// present; this state says whether that time is armed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleDisplayState {
    /// Enabled, not paused, and a next slot is a real scheduled evaluation.
    Scheduled,
    /// Definition `enabled` is false. A next slot, if present, is hypothetical.
    Disabled,
    /// Host-local pause. A next slot, if present, is hypothetical.
    Paused,
    /// Enabled delivery- or state-triggered work is waiting on that trigger.
    Waiting,
    /// The scheduler has never recorded a cursor for this definition.
    NeverObserved,
    /// Source or trigger state cannot be shown as a next evaluation, or this
    /// host can never admit the delivery work it would evaluate.
    Unavailable,
}

impl ScheduleDisplayState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Disabled => "disabled",
            Self::Paused => "paused",
            Self::Waiting => "waiting",
            Self::NeverObserved => "never_observed",
            Self::Unavailable => "unavailable",
        }
    }

    /// Disabled and paused rows may still carry a theoretical next slot.
    pub fn is_hypothetical(self) -> bool {
        matches!(self, Self::Disabled | Self::Paused)
    }

    /// The `{state, at, hypothetical}` next-evaluation record. A waiting
    /// trigger has no slot to show.
    pub fn next_evaluation(self, at: Option<String>) -> Value {
        json!({
            "state": self.as_str(),
            "at": if self == Self::Waiting { None } else { at },
            "hypothetical": self.is_hypothetical(),
        })
    }
}
