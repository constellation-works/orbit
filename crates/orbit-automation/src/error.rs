//! Typed failures from deterministic automation rules.

use orbit_common::OrbitError;

#[derive(Debug, thiserror::Error)]
pub enum AutomationError {
    #[error("invalid coverage evidence: {0}")]
    Evidence(String),
    #[error("automation deferred: {0}")]
    Deferred(String),
    #[error(transparent)]
    Boundary(#[from] OrbitError),
}

/// Translate once at the Core boundary, preserving invalid evidence as input errors.
pub fn automation_error_to_orbit(error: AutomationError) -> OrbitError {
    match error {
        AutomationError::Evidence(reason) => {
            OrbitError::InvalidInput(format!("coverage_evidence_invalid: {reason}"))
        }
        AutomationError::Deferred(reason) => {
            OrbitError::Execution(format!("automation_deferred: {reason}"))
        }
        AutomationError::Boundary(error) => error,
    }
}
