use std::process::Child;

use orbit_common::OrbitError;

use crate::runner::ExecRequest;

pub trait Sandbox {
    fn validate(&self, req: &ExecRequest) -> Result<(), OrbitError>;

    /// Create the child after [`Self::validate`] succeeds.
    ///
    /// The default launches through [`crate::process::spawn`]. Implementations
    /// that confine the child at the process boundary override this instead of
    /// wrapping the program in argv.
    fn spawn(&self, req: &ExecRequest) -> Result<Child, OrbitError> {
        crate::process::spawn(req)
    }
}

/// A sandbox strategy that adds no Orbit-specific validation or containment.
///
/// `NoSandbox` controls only the sandbox selection made by
/// [`run_process`](crate::run_process). It neither disables nor escapes an
/// outer sandbox already imposed on the parent process. For example, child
/// processes inherit macOS Seatbelt restrictions and Linux descendants inherit
/// their Bubblewrap mount namespace. It is therefore not a host-side or
/// credential boundary.
#[derive(Debug, Default)]
pub struct NoSandbox;

impl Sandbox for NoSandbox {
    fn validate(&self, _req: &ExecRequest) -> Result<(), OrbitError> {
        Ok(())
    }
}
