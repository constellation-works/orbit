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

use std::sync::Arc;

use crate::context::RunSettings;

/// The single capability boundary between the job executor and its runtime.
///
/// Deterministic actions, task/run persistence, environment resolution, agent
/// dispatch, and audit/checkpoint hooks all cross this boundary exactly once.
/// It declares no method of its own: each capability belongs to one of its
/// five supertraits, and the blanket implementation below makes any type that
/// implements all five a `RuntimeHost`.
///
/// Every method is required, so a new capability fails to compile until the
/// production host answers it. Full dispatch and job execution compose the
/// capability traits through this bound.
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

/// The run's resolved settings, held by the host for the run's life.
///
/// Composition resolves [`RunSettings`] once, when it builds the host for a
/// run, and the engine shares that value instead of asking the host to
/// resolve configuration at every read. Each field is fixed for the run's
/// life; an answer that can change during a run, or whose resolution does
/// work, is a [`HostEnvironment`] method instead. It is a supertrait of
/// [`HostEnvironment`], so every bound that reads the environment can read
/// the settings.
pub trait RuntimeHostRunSettings: Send + Sync {
    /// The settings composition resolved when it built this host, shared
    /// rather than resolved again.
    fn run_settings(&self) -> Arc<RunSettings>;
}
