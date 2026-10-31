//! Job names more than one application module dispatches on. Run records
//! persist these strings, so their values never change.

/// The job a follower drain runs.
pub const PULL_DRAIN_JOB: &str = "workspace_pull_pipeline";

/// The job the `pilot_warnings_held` and `block_escalated` triggers submit.
pub const ESCALATION_TRIAGE_JOB: &str = "escalation_triage_pipeline";

/// The job that verifies review-filed findings.
pub const FINDING_VERIFICATION_JOB: &str = "finding_verification_pipeline";

/// The job that delivers locally and therefore cannot honour `review.timing`
/// as a final route.
pub(crate) const LOCAL_ROUTE_JOB: &str = "task_local_pipeline";
