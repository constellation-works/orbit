//! How a task-pilot assessment holds or gates a task: backlog holds from
//! applied findings, prerequisite resolution and verified-no-diff holds.

mod admission;
mod blockers;
mod no_diff;

pub(crate) use admission::{
    HostOperationalHold, MachineRequirement, NativeOsHold, NativeOsRequirement,
    OperatorValidationHold, OperatorValidationRequirement, PilotAdmissionHold,
    operator_validation_requirements,
};
pub(crate) use blockers::{normalize_pilot_self_blockers, pilot_blocker_task_ids};
pub(crate) use no_diff::{NoDiffClosure, PILOT_VERIFIED_NO_DIFF, VerifiedNoDiff};
