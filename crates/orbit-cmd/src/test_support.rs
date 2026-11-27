//! Registered opens for downstream crates' tests, which seed and inspect a
//! workspace through what composition opened. A test turns the opening into
//! the runtime with `orbit_core`'s test-only `Opening::into_runtime`.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_core::app::workspace::Opening;
use orbit_runtime::OrbitRuntimeRoots;
use orbit_types::workspace::{Workspace, WorkspaceCheckout};

use crate::registry_runtime::RegisteredRuntimeFactory;

/// Open resolved roots with the registry context a command open attaches.
pub fn open_resolved_roots(roots: OrbitRuntimeRoots) -> Result<Opening, OrbitError> {
    RegisteredRuntimeFactory::open_resolved_roots(roots)
}

/// Open a registered checkout for writing.
pub fn open_registered_checkout(
    global_root: &Path,
    workspace: &Workspace,
    checkout: &WorkspaceCheckout,
) -> Result<Opening, OrbitError> {
    RegisteredRuntimeFactory::open_registered_checkout(global_root, workspace, checkout)
}

/// Open a registered checkout's existing state with no incidental writes.
pub fn open_registered_checkout_observational(
    global_root: &Path,
    workspace: &Workspace,
    checkout: &WorkspaceCheckout,
) -> Result<Opening, OrbitError> {
    RegisteredRuntimeFactory::open_registered_checkout_observational(
        global_root,
        workspace,
        checkout,
    )
}
