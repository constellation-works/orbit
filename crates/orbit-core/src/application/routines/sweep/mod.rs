//! The stateless scheduler tick [ORB-10021, ORB-12237]: what runs when the OS
//! clock invokes `orbit clock tick` (`orbit sweep` is a compatibility alias).
//! It never bootstraps a workspace from the caller's cwd, isolates per-routine
//! failures into report rows, and returns `Err` only for infrastructure
//! failures (registry unreadable, store unopenable) — an unconfigured host
//! is a clean no-op, because launchd/systemd will invoke this forever.

mod dispatch;
mod tick;
mod workspaces;

pub use orbit_automation::routines::sweep::{
    AutoTaskSweepReport, ExternalMergeSweepReport, RoutineDispatch, RoutineSweepReport,
    RunOwnerLiveness, SweepOptions, SweepOutcome,
};

#[cfg(any(test, feature = "test-support"))]
pub(crate) use tick::run_sweep_at_with_providers_at;
pub use tick::{run_sweep_at_with_providers, run_sweep_with_providers};
