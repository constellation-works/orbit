//! Compiled identity of this Orbit process, independent of its runtime checkout.
//!
//! The shipped `orbit` binary registers the value its build script embedded.
//! A process that never registers reports unknown provenance: this package's
//! version, no source commit, and `dirty: true`.

use std::sync::OnceLock;

use orbit_types::workflow::OrbitBuild;

static REGISTERED_ORBIT_BUILD: OnceLock<OrbitBuild> = OnceLock::new();

/// Record the compiled identity of the shipped binary. The first call wins.
pub fn register_orbit_build(build: OrbitBuild) {
    let _ = REGISTERED_ORBIT_BUILD.set(build);
}

/// Build identity of this process: the registered value, or unknown provenance.
pub fn current_orbit_build() -> OrbitBuild {
    REGISTERED_ORBIT_BUILD
        .get()
        .cloned()
        .unwrap_or_else(unknown_orbit_build)
}

fn unknown_orbit_build() -> OrbitBuild {
    OrbitBuild {
        version: env!("CARGO_PKG_VERSION").into(),
        source_commit: None,
        dirty: true,
    }
}
