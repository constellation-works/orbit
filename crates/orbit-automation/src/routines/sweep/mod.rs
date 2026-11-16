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
pub use orbit_clock::TICK_DEADLINE;
