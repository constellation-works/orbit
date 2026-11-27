//! The unified capability boundary between the engine and its runtime.
//!
//! The boundary is five traits, one per capability group: [`RunLifecycle`],
//! [`ReviewGate`], [`HostEnvironment`], [`Dispatch`] and [`Recovery`].
//! [`RuntimeHost`] is their supertrait, implemented for every type that
//! implements all five.

mod dispatch;
mod environment;
mod lifecycle;
mod recovery;
mod review;

pub use dispatch::Dispatch;
pub use environment::HostEnvironment;
pub use lifecycle::RunLifecycle;
pub use recovery::Recovery;
pub use review::ReviewGate;

/// The single capability boundary between the job executor and its runtime.
///
/// Deterministic actions, task/run persistence, environment resolution, agent
/// dispatch, and audit/checkpoint hooks all cross this boundary exactly once.
/// It declares no method of its own: each capability belongs to one of its
/// five supertraits, and the blanket implementation below makes any type that
/// implements all five a `RuntimeHost`.
///
/// Every method is required, so a new capability fails to compile until the
/// production host answers it. Test doubles implement
/// `test_support::NullRuntimeHost` instead, which answers each capability the
/// way a host without it does.
pub trait RuntimeHost:
    RunLifecycle
    + ReviewGate
    + HostEnvironment
    + Dispatch
    + Recovery
    + RuntimeHostProcessEnvironment
    + Send
    + Sync
{
}

impl<T> RuntimeHost for T where
    T: RunLifecycle
        + ReviewGate
        + HostEnvironment
        + Dispatch
        + Recovery
        + RuntimeHostProcessEnvironment
{
}

/// Required process environment capability for a [`RuntimeHost`].
///
/// Kept as a supertrait so the environment snapshot remains a required
/// capability without expanding the main runtime host method surface.
pub trait RuntimeHostProcessEnvironment: Send + Sync {
    /// Environment snapshot supplied by the embedding host.
    fn process_env(&self) -> orbit_common::Env;
}
