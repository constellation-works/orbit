use orbit_common::OrbitError;

pub(super) fn refused(message: impl Into<String>) -> OrbitError {
    OrbitError::PolicyDenied(message.into())
}
