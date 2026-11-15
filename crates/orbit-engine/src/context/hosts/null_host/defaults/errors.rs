use orbit_common::OrbitError;

use crate::activity_job::DispatchError;

pub(super) fn unsupported_runtime_capability(capability: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "runtime host capability '{capability}' is unavailable"
    ))
}

pub(super) fn unsupported_dispatch_capability(capability: &str) -> DispatchError {
    DispatchError::JobExecution(format!(
        "runtime host capability '{capability}' is unavailable"
    ))
}
