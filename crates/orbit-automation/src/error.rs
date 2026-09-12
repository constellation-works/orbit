//! Typed failures from deterministic automation rules.

use orbit_common::OrbitError;

#[derive(Debug, thiserror::Error)]
pub enum AutomationError {
    #[error("invalid coverage evidence: {0}")]
    Evidence(String),
    #[error("automation deferred: {0}")]
    Deferred(String),
    /// An explicit operator operation the current state forbids, naming every
    /// refusal that applies. Nothing was changed.
    #[error("recovery refused: {0}")]
    Refused(String),
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
        AutomationError::Refused(reasons) => {
            OrbitError::InvalidInput(format!("recovery_refused: {reasons}"))
        }
        AutomationError::Boundary(error) => error,
    }
}
