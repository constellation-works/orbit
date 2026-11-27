//! Engine-host adapter access the application reaches through a port the
//! adapter implements.

use orbit_engine::RuntimeHost;

/// Engine-host adapter access for application use cases.
///
/// The adapter implements this port on `OrbitRuntime` by returning
/// `EngineHost`, so application modules pass the engine-host adapter to
/// `orbit-engine` entry points without importing `crate::adapter`.
pub(crate) trait EngineHostPort {
    /// Concrete engine-host adapter bound to `self`.
    type Host<'a>: RuntimeHost + 'a
    where
        Self: 'a;

    /// Build the engine-host adapter bound to this runtime.
    fn engine_host(&self) -> Self::Host<'_>;
}
