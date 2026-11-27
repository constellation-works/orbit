use orbit_common::OrbitError;

use crate::context::dispatch_contract::DispatchError;

/// Refuse a capability the fixture cannot provide.
pub fn unsupported_runtime_capability(capability: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "runtime host capability '{capability}' is unavailable"
    ))
}

/// Refuse a dispatch capability the fixture cannot provide.
pub fn unsupported_dispatch_capability(capability: &str) -> DispatchError {
    DispatchError::JobExecution(format!(
        "runtime host capability '{capability}' is unavailable"
    ))
}
