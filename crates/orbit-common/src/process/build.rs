//! Compiled identity of this Orbit process, independent of its runtime checkout.

use orbit_types::workflow::OrbitBuild;

/// Build identity embedded during compilation, never read from the runtime environment.
pub fn current_orbit_build() -> OrbitBuild {
    OrbitBuild {
        version: env!("CARGO_PKG_VERSION").into(),
        source_commit: match env!("ORBIT_BUILD_SOURCE_COMMIT") {
            "" => None,
            commit => Some(commit.into()),
        },
        dirty: env!("ORBIT_BUILD_DIRTY") != "false",
    }
}
