//! Durable run state: the pipeline snapshot and the drain, recovery and
//! failure-classification records it carries.

mod drain;
mod failure;
mod pipeline;
mod pipeline_serialization;
mod recovery;
mod steering;

pub use drain::{
    CrewExclusion, CrewExclusionSource, DrainAdmissionForecast, DrainAdmissionPass,
    DrainAdmissionsStop, DrainApprovalReport, DrainCancelRequest, DrainCapacity, DrainWaitingTask,
    DrainWindowExtension, DrainWorkerLimit, ForecastPeak, GapFitAdmission, GapFitHead,
    GapFitHeadMiss, GapFitHeadStall, GapFitSkip, PullAdmissionFit, PullAuthExclusion,
    PullAuthRecovery, PullBreaker, PullCrewPreflight, PullDrainState, PullSinglePass,
    PullUnfitWork, PullWindowClose, ResourcePressure, ResourceThrottle, TaskCancellationPolicy,
};
pub use failure::{
    CONTRACT_REFUSAL_MARKER, ClaimFailureClass, OWNER_BUSY_MARKER,
    OWNER_ROUTE_UNAVAILABLE_ERROR_CODE, OWNER_ROUTE_UNAVAILABLE_MARKER,
    PROVIDER_CAPACITY_ERROR_CODE, PROVIDER_CAPACITY_MARKER, PROVIDER_LIMIT_ERROR_CODE,
    PROVIDER_LIMIT_MARKER, PROVIDER_REFUSAL_ERROR_CODE, PROVIDER_REFUSAL_MARKER,
    PROVIDER_UNAVAILABLE_ERROR_CODE, PROVIDER_UNAVAILABLE_MARKER, STALLED_ERROR_CODE,
    STALLED_MARKER, TRANSIENT_FAILURE_ERROR_CODE, TRANSIENT_FAILURE_MARKER,
    VALIDATION_ENVIRONMENT_ERROR_CODE, VALIDATION_ENVIRONMENT_MARKER, is_contract_refusal,
    is_owner_busy, is_owner_route_unavailable, is_provider_capacity_exhausted, is_provider_failure,
    is_provider_limit, is_provider_refusal, is_provider_unavailable,
    is_validation_environment_failure,
};
pub use pipeline::PipelineState;
pub use recovery::{
    ActivityCrewDraw, ActivityCrewPoolMember, FailureActivityCheckpoint, FinalRecoveryCheckpoint,
    FinalRecoveryKey, FinalRecoveryObservedTask, FinalRecoveryRepairCommit, FinalRecoveryResume,
};
pub use steering::{
    CommentLogMark, STEERING_UNCONSUMED_EVENT, STEERING_UNCONSUMED_MARKER, SteeringRound,
    SteeringWatermark, StepSteeringSnapshot,
};
