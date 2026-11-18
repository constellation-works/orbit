//! The one table of step failures no repair agent can repair.
//!
//! Step recovery and final recovery stop on these classes; they used to keep
//! separate copies of the list. Add a new "do not spend a repair agent on
//! this" class here. The retry wrapper tests its own stop conditions
//! directly: the table returns the first matching class, and a blocker's
//! free-text evidence may quote another class's marker.

use orbit_types::workflow::StepFailureCode;

/// A step failure class no repair agent can repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::activity_job::job_executor) enum UnrepairableFailure {
    /// The provider reported the selected model at capacity.
    ProviderCapacityExhausted,
    /// The provider account hit its usage limit.
    ProviderLimit,
    /// The provider could not be used on this host.
    ProviderUnavailable,
    /// Muse refused access to the selected model.
    MuseModelAccessRefused,
    /// The provider's content policy refused the turn.
    ProviderRefusal,
    /// Required validation could not run in the lane that tried it.
    ValidationRouteUnavailable,
    /// Required validation lacked a tool in its environment.
    ValidationEnvironment,
    /// A claimed worker could not reach its owner.
    OwnerRouteUnavailable,
    /// Recorded owner approval is still missing.
    OwnerSignoffRequired,
    /// Required validation fails on the base exactly as on the candidate.
    BaselineRed,
    /// An agent already declared a blocker.
    BlockedByAgent,
    /// An Orbit upgrade refused the step's agent.
    UpgradePending,
    /// A resumed published delivery's deferred footprint stayed held past
    /// its wait; no editor started.
    FootprintUnavailable,
}

/// The unrepairable class a step failure carries, if any.
///
/// Tests in the order `attempt_final_recovery` used when the lists were
/// kept separately: finer classes before the broader ones they are kinds
/// of (validation route before validation environment, provider capacity
/// and limit before provider unavailable). The typed `code` decides
/// first where a classifier accepts one; a failure with no typed code
/// falls back to the legacy markers inside the classifiers. The four
/// provider predicates stay separate calls: `ProviderFailureClass::of`
/// orders refusal before capacity and limit, which would change which
/// skip text a failure carrying several markers gets.
#[must_use]
pub(in crate::activity_job::job_executor) fn unrepairable_failure(
    code: Option<&StepFailureCode>,
    message: Option<&str>,
) -> Option<UnrepairableFailure> {
    let error_code = code.map(StepFailureCode::as_str);
    if orbit_types::workflow::is_validation_route_unavailable(error_code, message) {
        Some(UnrepairableFailure::ValidationRouteUnavailable)
    } else if orbit_types::workflow::is_validation_environment_failure(error_code, message) {
        Some(UnrepairableFailure::ValidationEnvironment)
    } else if orbit_types::workflow::is_baseline_red_failure(error_code, message) {
        Some(UnrepairableFailure::BaselineRed)
    } else if orbit_types::workflow::is_task_blocked_by_agent(error_code, message) {
        Some(UnrepairableFailure::BlockedByAgent)
    } else if orbit_types::workflow::is_upgrade_pending(error_code, message) {
        Some(UnrepairableFailure::UpgradePending)
    } else if code == Some(&StepFailureCode::MuseModelAccessRefused) {
        Some(UnrepairableFailure::MuseModelAccessRefused)
    } else if orbit_types::workflow::is_provider_capacity_exhausted(error_code, message) {
        Some(UnrepairableFailure::ProviderCapacityExhausted)
    } else if orbit_types::workflow::is_provider_limit(error_code, message) {
        Some(UnrepairableFailure::ProviderLimit)
    } else if orbit_types::workflow::is_provider_unavailable(error_code, message) {
        Some(UnrepairableFailure::ProviderUnavailable)
    } else if orbit_types::workflow::is_provider_refusal(error_code, message) {
        Some(UnrepairableFailure::ProviderRefusal)
    } else if orbit_types::workflow::is_owner_route_unavailable(code, message) {
        Some(UnrepairableFailure::OwnerRouteUnavailable)
    } else if orbit_types::workflow::is_owner_signoff_required(error_code, message) {
        Some(UnrepairableFailure::OwnerSignoffRequired)
    } else if orbit_types::workflow::is_footprint_unavailable(error_code, message) {
        Some(UnrepairableFailure::FootprintUnavailable)
    } else {
        None
    }
}
