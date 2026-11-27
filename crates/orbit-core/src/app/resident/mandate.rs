//! Resident mandate state and effective authority.
//!
//! A mandate is an assignment task tagged `resident` with a
//! `resident/mandate.json` artifact. Its authority lives in the task's
//! history as `resident_authority_changed` entries
//! ([`orbit_types::workflow::ResidentAuthority`]).

use orbit_types::task::TaskStatus;
use orbit_types::workflow::ResidentRight;
use serde::Serialize;

/// The job every workspace's seeded resident cycle routine targets.
pub(crate) const RESIDENT_CYCLE_ROUTINE_TARGET: &str = "job:resident_cycle_pipeline";

/// Where a mandate stands, as `orbit resident show` reports it.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResidentMandateState {
    /// Tagged, with a valid mandate, and its assignment still open.
    Active,
    /// The assignment is in review: the resident handed off.
    HandedOff,
    /// Paused by `orbit resident control pause`: no new cycle.
    Paused,
    /// Taken over by a person: no cycle fires until `release`.
    TakenOver,
    /// The assignment is done, archived or rejected, or the mandate was
    /// stopped.
    Ended,
    /// Not tagged `resident`, or the mandate artifact is missing or invalid:
    /// the mandate is not due.
    Invalid,
}

/// The rights and workspaces a cycle could act on now: the authority's
/// rights, scoped to workspaces both the authority and the mandate cover.
/// Empty (observe-only) when either record is unavailable.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ResidentEffectiveAuthority {
    /// Rights held, in contract order.
    pub rights: Vec<ResidentRight>,
    /// Workspaces in scope.
    pub workspaces: Vec<String>,
}

/// Why a mandate cannot fire while its workspace cycle routine is disabled.
pub(crate) fn resident_cycle_routine_disabled_reason(routine_name: &str) -> String {
    format!(
        "the workspace's resident cycle routine `{routine_name}` is disabled, so no resident cycle will fire until it is enabled"
    )
}

/// Whether a task status ends its mandate.
pub(crate) fn resident_mandate_ended(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Done | TaskStatus::Archived | TaskStatus::Rejected
    )
}
