//! Row and parameter types for the host-local routine scheduler state.

use serde::{Deserialize, Serialize};

/// Lifecycle of one fire attempt. `Intent` and `Dispatched` are the
/// non-terminal states [`crate::Store::routine_unresolved_fires`] returns;
/// everything else is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutineFireState {
    /// Claimed (idempotency key written) but not yet handed to the run
    /// machinery. A crashed sweep can leave a fire here.
    Intent,
    /// Submitted to the run machinery; `run_id` is set.
    Dispatched,
    /// The dispatched run finished successfully.
    Succeeded,
    /// The scheduled slot was consumed without dispatch. A workspace drain
    /// already owned the coordinator slot, or `missed_run: skip` recorded a
    /// gap. `detail` carries the reason.
    Skipped,
    /// The dispatched run finished in failure.
    Failed,
    /// The fire exceeded the routine's `policy.timeout_minutes` without a
    /// terminal run outcome and no longer blocks `overlap: forbid`.
    TimedOut,
    /// Dispatch itself errored (e.g. target no longer resolvable).
    Error,
}

impl RoutineFireState {
    /// Stable string form persisted in the `routine_fires.state` column.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Dispatched => "dispatched",
            Self::Succeeded => "succeeded",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Error => "error",
        }
    }

    /// Inverse of [`Self::as_str`]; `None` for unknown values.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "intent" => Some(Self::Intent),
            "dispatched" => Some(Self::Dispatched),
            "succeeded" => Some(Self::Succeeded),
            "skipped" => Some(Self::Skipped),
            "failed" => Some(Self::Failed),
            "timed_out" => Some(Self::TimedOut),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    /// Whether this state ends the fire's lifecycle.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Intent | Self::Dispatched)
    }
}

/// Per-routine scheduling cursor on this host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineCursor {
    /// Routine this cursor belongs to.
    pub routine_name: String,
    /// RFC 3339 timestamp of this host's first observation of the routine.
    /// Slots scheduled before it never fire.
    pub baseline_at: String,
    /// RFC 3339 timestamp of the last scheduled slot consumed (fired or
    /// claimed), if any.
    pub last_slot: Option<String>,
}

/// Parameters for recording a fire intent (the idempotency claim).
#[derive(Debug, Clone)]
pub struct RoutineFireIntentParams {
    /// Routine being fired.
    pub routine_name: String,
    /// RFC 3339 timestamp of the scheduled slot being consumed.
    pub slot: String,
    /// 1-based attempt number (retries increment it under the same slot).
    pub attempt: u32,
    /// Name of the source workspace the definition was loaded from.
    pub source_workspace: String,
}

/// Attempt occupancy for one scheduled slot.
///
/// `highest` includes skipped rows, which occupy the idempotency key.
/// `non_skipped` counts real tries. A drain yield that recorded `skipped`
/// does not spend `retries.max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutineSlotAttempts {
    /// Largest attempt number stored for the slot, skipped rows included.
    pub highest: u32,
    /// How many of those rows are not `skipped`.
    pub non_skipped: u32,
}

/// One recorded fire attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineFireRecord {
    /// Routine that fired.
    pub routine_name: String,
    /// RFC 3339 timestamp of the scheduled slot this fire consumed.
    pub slot: String,
    /// 1-based attempt number under this slot.
    pub attempt: u32,
    /// Current lifecycle state.
    pub state: RoutineFireState,
    /// Run id returned by the run machinery once dispatched.
    pub run_id: Option<String>,
    /// Source workspace name the definition was loaded from.
    pub source_workspace: String,
    /// Optional detail (dispatch error, outcome message).
    pub detail: Option<String>,
    /// RFC 3339 creation (intent) timestamp.
    pub created_at: String,
    /// RFC 3339 timestamp of the last state change.
    pub updated_at: String,
}

/// One host-local pause row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutinePauseRecord {
    /// Paused routine.
    pub routine_name: String,
    /// RFC 3339 timestamp the pause was written.
    pub paused_at: String,
    /// Who paused it (actor label), when recorded.
    pub actor: Option<String>,
}

/// Whether a routine's circuit breaker lets the sweep fire it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutineBreakerState {
    /// Fires normally. A closed row only keeps the floor below which older
    /// failures no longer count toward a trip.
    Closed,
    /// Tripped: the sweep fires the routine only for one trial.
    Open,
}

impl RoutineBreakerState {
    /// Stable string form persisted in the `routine_breakers.state` column.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open => "open",
        }
    }

    /// Inverse of [`Self::as_str`]; `None` for unknown values.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "closed" => Some(Self::Closed),
            "open" => Some(Self::Open),
            _ => None,
        }
    }
}

/// Host-local circuit-breaker state of one routine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineBreakerRecord {
    /// Routine this breaker guards.
    pub routine_name: String,
    /// Source workspace name the routine was loaded from when last written.
    pub source_workspace: String,
    /// Closed or open.
    pub state: RoutineBreakerState,
    /// Error class (failure code or kind) of the failures that tripped it.
    pub error_class: Option<String>,
    /// Run of the newest failure that tripped or re-opened it, when one ran.
    pub last_run_id: Option<String>,
    /// RFC 3339 time of the latest trip or re-open.
    pub tripped_at: Option<String>,
    /// Trips since the breaker last closed; it sets the trial backoff.
    pub trips: u32,
    /// RFC 3339 time the next trial is allowed by backoff.
    pub next_trial_at: Option<String>,
    /// Orbit build identity on this host at the latest trip; a different
    /// build allows a trial at once.
    pub orbit_build: Option<String>,
    /// RFC 3339 time the current trial was admitted; its first fire decides
    /// whether the breaker closes or re-opens.
    pub trial_started_at: Option<String>,
    /// Why the current trial was admitted: `backoff`, `version` or `bypass`.
    pub trial_reason: Option<String>,
    /// RFC 3339 time an operator asked for one fire now.
    pub bypass_requested_at: Option<String>,
    /// Fires created at or before this RFC 3339 time never count toward a
    /// new trip (set when the breaker closes or is reset).
    pub floor_at: Option<String>,
    /// RFC 3339 time of the last write.
    pub updated_at: String,
}

/// One recorded breaker transition or operator action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineBreakerEvent {
    /// Routine the event belongs to.
    pub routine_name: String,
    /// `tripped`, `trial`, `closed`, `reopened`, `reset` or `bypass`.
    pub event: String,
    /// Who acted: `sweep` for the scheduler, else the operator label.
    pub actor: String,
    /// Error class, trial reason or other short detail.
    pub detail: Option<String>,
    /// Run the event refers to, when one did.
    pub run_id: Option<String>,
    /// RFC 3339 time it was recorded.
    pub at: String,
}
