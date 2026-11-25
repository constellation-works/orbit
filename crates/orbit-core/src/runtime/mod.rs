//! The runtime kernel, re-exported from `orbit-runtime`, and the public
//! [`OrbitRuntime`] that wraps it.
//!
//! `crate::runtime::…` and `orbit_core::runtime::…` paths resolve to the
//! kernel crate's items. [`OrbitRuntime`] owns a [`RuntimeKernel`] and
//! dereferences to it, so kernel methods are called on it directly, while
//! application and adapter methods stay inherent on this crate's type. The
//! kernel reaches those methods only through [`ApplicationPort`], which
//! composition implements.

use std::ops::{Deref, DerefMut};
use std::sync::OnceLock;

use orbit_common::Env;
pub use orbit_runtime::*;

mod builders;
mod forwarders;

#[cfg(test)]
mod tests;

/// The environment `main` captured, kept where runtimes and core operations
/// resolve it.
static CAPTURED_ENV: OnceLock<Env> = OnceLock::new();

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

/// The fully initialized Orbit runtime: the [`RuntimeKernel`] plus the
/// application and adapter operations defined on it in this crate.
#[derive(Clone)]
// Transparent, so a kernel reference can be viewed as the runtime that owns it.
#[repr(transparent)]
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
    pub(crate) fn from_kernel(kernel: RuntimeKernel) -> Self {
        Self { kernel }
    }

    /// The kernel this runtime owns, for a kernel interface such as
    /// [`WorkspaceCatalog::open`](workspace::catalog::WorkspaceCatalog::open)
    /// that returns one.
    pub fn into_kernel(self) -> RuntimeKernel {
        self.kernel
    }

    /// View a kernel a kernel interface lent back, such as the runtime a
    /// [`WorkspaceCatalog`](workspace::catalog::WorkspaceCatalog) hook receives,
    /// as the runtime that owns it.
    pub fn from_kernel_ref(kernel: &RuntimeKernel) -> &Self {
        // SAFETY: `OrbitRuntime` is `repr(transparent)` over its only field,
        // a `RuntimeKernel`, so both types share one layout and the cast
        // keeps the reference's lifetime and validity.
        unsafe { &*(kernel as *const RuntimeKernel).cast::<Self>() }
    }
}
