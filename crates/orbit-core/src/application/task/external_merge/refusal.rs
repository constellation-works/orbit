use orbit_common::OrbitError;

pub(super) fn refused(id: &str, reason: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "task '{id}' does not settle from a merged delivery pull request: {reason}"
    ))
}
