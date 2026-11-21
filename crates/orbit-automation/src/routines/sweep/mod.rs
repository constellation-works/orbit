//! Deterministic routine evaluation, retry and overlap coordination.

mod contract;
mod core;
mod fire;
mod missed_slots;
mod report;
mod retry;

pub use contract::{
    AutoTaskSweepReport, ExternalMergeSweepReport, RoutineDispatch, RoutineSweepReport,
    RunOwnerLiveness, SweepOptions, SweepOutcome,
};
pub use core::{duplicate_ownership_skipped, run_sweep_core};
/// Cooperative budget for a host scheduler tick, owned by the clock whose
/// health judges an overdue tick against it [ORB-22688].
pub use orbit_clock::TICK_DEADLINE;
