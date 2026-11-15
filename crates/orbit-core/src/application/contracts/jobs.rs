//! Job names more than one application module dispatches on. Run records
//! persist these strings, so their values never change.

/// The job a follower drain runs.
pub const PULL_DRAIN_JOB: &str = orbit_types::workflow::WORKSPACE_PULL_PIPELINE_JOB;

/// The job the `pilot_warnings_held` trigger submits.
pub const ESCALATION_TRIAGE_JOB: &str = "escalation_triage_pipeline";

/// The job that verifies review-filed findings.
pub const FINDING_VERIFICATION_JOB: &str = "finding_verification_pipeline";
