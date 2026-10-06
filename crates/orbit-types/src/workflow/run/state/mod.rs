//! Durable run state: the pipeline snapshot and the drain, recovery and
//! failure-classification records it carries.

mod drain;
mod failure;
mod pipeline;
mod recovery;

#[cfg(test)]
mod tests;

pub use drain::{
    CrewExclusion, CrewExclusionSource, DrainAdmissionPass, DrainAdmissionsStop,
    DrainApprovalReport, DrainCancelRequest, DrainWaitingTask, DrainWorkerLimit, PullCrewPreflight,
    PullSinglePass, ResourcePressure, ResourceThrottle, TaskCancellationPolicy,
};
pub use failure::{
    ClaimFailureClass, OWNER_ROUTE_UNAVAILABLE_ERROR_CODE, OWNER_ROUTE_UNAVAILABLE_MARKER,
    PROVIDER_CAPACITY_ERROR_CODE, PROVIDER_CAPACITY_MARKER, PROVIDER_REFUSAL_ERROR_CODE,
    PROVIDER_REFUSAL_MARKER, PROVIDER_UNAVAILABLE_ERROR_CODE, PROVIDER_UNAVAILABLE_MARKER,
    TRANSIENT_FAILURE_ERROR_CODE, TRANSIENT_FAILURE_MARKER, VALIDATION_ENVIRONMENT_ERROR_CODE,
    VALIDATION_ENVIRONMENT_MARKER, is_owner_route_unavailable, is_provider_capacity_exhausted,
    is_provider_failure, is_provider_refusal, is_provider_unavailable,
    is_validation_environment_failure,
};
pub use pipeline::PipelineState;
pub use recovery::{
    ActivityCrewDraw, ActivityCrewPoolMember, FailureActivityCheckpoint, FinalRecoveryCheckpoint,
    FinalRecoveryKey, FinalRecoveryObservedTask,
};
