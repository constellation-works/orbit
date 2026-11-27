//! The runtime kernel, re-exported from `orbit-runtime`, and the
//! [`OrbitRuntime`] that wraps it.
//!
//! `crate::runtime::…` paths resolve to the kernel crate's items; outside
//! this crate they are reachable only under the dev-only `test-support`
//! feature, as `orbit_core::runtime::…`. [`OrbitRuntime`] owns a [`RuntimeKernel`] and
//! dereferences to it, so kernel methods are called on it directly, while
//! application and adapter methods stay inherent on this crate's type. The
//! kernel never calls them: a mechanism that needs a use case's answer takes
//! it as an argument, or the flow that needs both lives in this crate.

mod builders;
mod forwarders;

mod permalink;

use std::ops::{Deref, DerefMut};
use std::sync::OnceLock;

use orbit_common::Env;
#[cfg(feature = "test-support")]
pub use orbit_runtime::*;
#[cfg(not(feature = "test-support"))]
pub(crate) use orbit_runtime::*;

#[cfg(test)]
mod tests;

/// The environment `main` captured, kept where runtimes and core operations
/// resolve it.
static CAPTURED_ENV: OnceLock<Env> = OnceLock::new();

/// The fully initialized Orbit runtime: the [`RuntimeKernel`] plus the
/// application and adapter operations defined on it in this crate.
#[derive(Clone)]
pub struct OrbitRuntime {
    kernel: RuntimeKernel,
}

impl Deref for OrbitRuntime {
    type Target = RuntimeKernel;

    fn deref(&self) -> &RuntimeKernel {
        &self.kernel
    }
}

impl DerefMut for OrbitRuntime {
    fn deref_mut(&mut self) -> &mut RuntimeKernel {
        &mut self.kernel
    }
}

impl OrbitRuntime {
    /// The runtime that owns `kernel`, for a kernel a kernel interface
    /// handed over: the one
    /// [`WorkspaceCatalog::open`](workspace::catalog::WorkspaceCatalog::open)
    /// returned, or a clone of the one a
    /// [`WorkspaceCatalog`](workspace::catalog::WorkspaceCatalog) hook was lent.
    pub fn from_kernel(kernel: RuntimeKernel) -> Self {
        Self { kernel }
    }

    /// Build the exact task permalink `<origin>/t/<id>` when
    /// `machine.web.public_url` is configured on this runtime's machine.
    pub(crate) fn task_permalink_url(&self, task_id: &str) -> Option<String> {
        permalink::build_task_permalink(&self.global_root(), task_id)
    }

    /// Build the exact run permalink `<origin>/r/<id>` (with `?machine=` and
    /// `&workspace=` when known) using `run.executed_on.machine_id` or local
    /// machine identity when `machine.web.public_url` is configured.
    pub(crate) fn run_permalink_url(&self, run: &orbit_types::workflow::JobRun) -> Option<String> {
        self.run_permalink_for_location(&run.run_id, run.executed_on.as_ref(), None)
    }

    /// Build the exact run permalink for `run_id` using an optional execution
    /// location and optional workspace override.
    pub(crate) fn run_permalink_for_location(
        &self,
        run_id: &str,
        executed_on: Option<&orbit_types::task::ExecutionLocation>,
        workspace_override: Option<&str>,
    ) -> Option<String> {
        let machine_id = self.resolve_run_machine_id(executed_on);
        let workspace_id = self.resolve_run_workspace_id(workspace_override);
        permalink::build_run_permalink(
            &self.global_root(),
            run_id,
            machine_id.as_deref(),
            workspace_id.as_deref(),
        )
    }

    fn resolve_run_machine_id(
        &self,
        executed_on: Option<&orbit_types::task::ExecutionLocation>,
    ) -> Option<String> {
        executed_on
            .map(|loc| loc.machine_id.trim())
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                self.automation_machine_identity()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
            })
            .or_else(|| {
                orbit_registry::inspect_machine_identity(&self.global_root())
                    .ok()
                    .and_then(|identity| {
                        identity
                            .id()
                            .map(str::trim)
                            .filter(|id| !id.is_empty())
                            .map(str::to_owned)
                    })
            })
    }

    fn resolve_run_workspace_id(&self, workspace_override: Option<&str>) -> Option<String> {
        workspace_override
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                self.opened_registered_workspace()
                    .flatten()
                    .map(|(id, _)| id.trim())
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
            })
            .or_else(|| {
                self.workspace_id()
                    .ok()
                    .map(|id| id.trim().to_owned())
                    .filter(|id| !id.is_empty())
            })
    }

    /// The kernel this runtime owns, for a kernel interface such as
    /// [`WorkspaceCatalog::open`](workspace::catalog::WorkspaceCatalog::open)
    /// that returns one.
    pub fn into_kernel(self) -> RuntimeKernel {
        self.kernel
    }
}

/// Keep the environment `main` captured with [`Env::capture`] for
/// composition. The first snapshot is retained. A repeated call fails a
/// debug assertion in debug builds; in release builds, it returns without
/// replacing that snapshot.
pub fn install_captured_env(env: Env) {
    let installed = CAPTURED_ENV.set(env).is_ok();
    debug_assert!(
        installed,
        "the process environment is captured once, in main"
    );
}

/// The environment `main` captured, or `None` in a process that never ran
/// `main`, such as a test binary.
pub fn captured_env() -> Option<&'static Env> {
    CAPTURED_ENV.get()
}

/// The environment captured by Orbit's entry point, or the current process
/// environment for a process without an installed entry-point snapshot.
pub(crate) fn process_env() -> Env {
    captured_env().cloned().unwrap_or_else(Env::capture)
}
