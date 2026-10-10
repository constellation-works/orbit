//! Task commands and coordinated record writes.

mod add;
mod baseline_hold;
mod blocked_recovery;
pub(crate) mod contention;
mod context_repair;
mod context_widening;
mod desktop;
mod provider_hold;
pub(crate) mod provider_limit;
mod provider_limit_ledger;
mod provider_limit_view;
pub(crate) use desktop::HandoffPullRequest;
mod final_recovery;
mod helpers;
mod lifecycle;
mod lint;
mod listing;
mod params;
mod paths;
mod pilot_admission;
mod pilot_no_diff;
mod pr_closure;
mod query;
mod records;
mod resolves;
mod retained_candidate;
mod transitions;
mod update;
pub(crate) mod validation_tools;

/// The shared non-pruning footprint calculation. Task reads, projections,
/// reservations, status-derived locks, and the admission work that freezes a
/// claim's footprint all resolve declarations through it.
pub use crate::runtime::task::{DeclaredContextFiles, declared_context_files};
pub(crate) use baseline_hold::refresh_baseline_holds_step;
pub use baseline_hold::{BASELINE_HOLD_REFRESH_JOB, BaselineHoldRefresh};
pub(crate) use blocked_recovery::{
    BACKSTOP_DECISIONS, BACKSTOP_LANE_CONTRACT, BlockedRecoveryPreparation, recovery_checkout_path,
};
pub use blocked_recovery::{
    BLOCKED_TASK_RECOVERY_JOB, BlockEpisode, BlockSource, BlockedRecoveryInput,
    BlockedRecoveryTick, BlockedRecoveryView, EpisodeDisposition, FinalRecoveryRecord,
    MAX_ACTIVE_BLOCKED_RECOVERIES, MAX_EPISODE_AGE_HOURS, episode_disposition,
};
pub use contention::{LockContentionHotspot, LockContentionReport};
pub use context_repair::ContextFileRestoration;
pub use final_recovery::{
    FINAL_RECOVERY_REQUEUED_EVENT, FinalRecoveryCompletion, FinalRecoveryOutcome,
    FinalRecoveryRequest, FinalRecoveryRequeueBound, FinalRecoveryTaskRevision,
};
pub use lint::{TaskLintFinding, TaskLintReport, TaskLintSeverity};
pub use listing::{
    TaskCandidateKey, TaskCandidateKeys, TaskCandidates, TaskListFilter, TaskListQuery, TaskPage,
    TaskRow,
};
pub(crate) use listing::{TaskEligibilityQuery, list_task_metadata_in};
pub(crate) use params::TaskRecordUpdateParams;
pub use params::{TaskAddParams, TaskUpdateParams};
pub use paths::ContextCreationAuthorization;
pub(crate) use pilot_admission::{
    HostOperationalHold, NativeOsHold, NativeOsRequirement, OperatorValidationHold,
    OperatorValidationRequirement, PilotAdmissionHold, operator_validation_requirements,
};
pub(crate) use pilot_no_diff::{NoDiffClosure, PILOT_VERIFIED_NO_DIFF, VerifiedNoDiff};
pub use provider_limit_view::{
    CrewProviderLimit, ProviderLimitReading, ProviderLimitsView, ProviderUsageStatus,
    UngatedLaneLimit, short_time,
};
pub(crate) use validation_tools::positive_validation_tools;

pub(crate) use helpers::{SYSTEM_ACTOR_LABEL, TaskAttributionInput, assemble_task_attribution};
pub(crate) use lifecycle::{
    ensure_completion_run_stopped, ensure_task_has_execution_plan,
    in_progress_transition_requires_plan, is_operator_rescue_close,
};
pub use lifecycle::{task_status_transition_allowed, task_status_transition_required_field};
pub(crate) use paths::{compute_task_add_warnings, context_workspace_root};

#[cfg(test)]
mod tests;
