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
    /// The scheduled slot was consumed without dispatch because a workspace
    /// drain already owned the coordinator slot.
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
