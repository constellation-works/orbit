//! Resident orchestrator mandates: the mandate record, its authority and the
//! reads over both.
//!
//! A mandate is an assignment task tagged `resident` with a
//! `resident/mandate.json` artifact. Its authority lives in the task's
//! history as `resident_authority_changed` entries
//! ([`orbit_types::workflow::ResidentAuthority`]), written only here:
//! [`crate::OrbitRuntime::grant_resident_authority`] applies an operator's widening,
//! turns anyone else's into a `resident_authority` decision request, and
//! applies any task writer's narrowing; the decision answer path applies a
//! human's `grant` through `OrbitRuntime::apply_resident_grant_answer`, which
//! task reaches only through the task-owned `TaskResidentPort` that
//! `task_port` implements, as it does resident comment admission.
//! A cycle fires through the `resident_due` state trigger (`due`), runs
//! as `resident_cycle_pipeline` (`steps`) and reaches tools only through
//! the cycle guard (`guard`). The manager pauses, stops, wakes or takes a
//! mandate over and relays the human's words to it through `control`. Core
//! alone moves the assignment's status across the mandate's life (`assignment`).

mod assignment;
mod comments;
mod control;
mod create;
mod cycle;
mod digest;
mod due;
mod grant;
mod guard;
mod mandate_reads;
mod metrics;
mod routine_status_port;
mod steps;
mod task_port;
mod view;

#[cfg(feature = "test-support")]
pub use cycle::test_support::with_resident_cycle_admission_checkpoint;

use orbit_types::task::TaskStatus;
use orbit_types::workflow::ResidentRight;
use serde::Serialize;

pub(crate) use comments::ensure_resident_comment_redaction_safe;
pub use control::{
    ResidentChannelCaller, ResidentControlAction, ResidentControlParams, ResidentControlState,
    ResidentRelayParams,
};
pub use create::ResidentMandateCreateParams;
pub(crate) use due::ResidentDue;
pub(crate) use grant::resident_grant_params_from_input;
pub use grant::{
    ResidentDashboardGrant, ResidentDashboardGrantOutcome, ResidentDashboardGrantParams,
    ResidentGrantCaller, ResidentGrantOutcome, ResidentGrantParams,
};
pub(crate) use guard::{ResidentCallAdmission, ResidentCallClass};
pub(in crate::application) use routine_status_port::ResidentRoutineStatusPort;
pub use steps::{RESIDENT_CYCLE_LEASE_MINUTES, resident_cycle_id};

pub(crate) use view::{ResidentView, resident_mandate_from_artifact};

/// Where a mandate stands, as `orbit resident show` reports it.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResidentMandateState {
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

/// The job every workspace's seeded resident cycle routine targets.
pub(crate) const RESIDENT_CYCLE_ROUTINE_TARGET: &str = "job:resident_cycle_pipeline";

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
