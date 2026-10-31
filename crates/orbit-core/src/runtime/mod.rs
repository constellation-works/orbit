//! The runtime kernel, re-exported from `orbit-runtime`, and the public
//! [`OrbitRuntime`] that wraps it.
//!
//! `crate::runtime::…` and `orbit_core::runtime::…` paths resolve to the
//! kernel crate's items. [`OrbitRuntime`] owns a [`RuntimeKernel`] and
//! dereferences to it, so kernel methods are called on it directly, while
//! application and adapter methods stay inherent on this crate's type. The
//! kernel reaches those methods only through [`ApplicationPort`], which this
//! module implements.

use std::ops::{Deref, DerefMut};

pub(crate) use application_port::APPLICATION_PORT;
pub use orbit_runtime::*;

mod application_port;
mod builders;
mod forwarders;

#[cfg(test)]
mod tests;

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

    fn from_kernel_ref(kernel: &RuntimeKernel) -> &Self {
        // SAFETY: `OrbitRuntime` is `repr(transparent)` over its only field,
        // a `RuntimeKernel`, so both types share one layout and the cast
        // keeps the reference's lifetime and validity.
        unsafe { &*(kernel as *const RuntimeKernel).cast::<Self>() }
    }
}
