//! Routines [ORB-10021]: Orbit as the constellation's single scheduler.
//!
//! A routine is a durable, git-versioned YAML definition of recurring work —
//! a cron trigger, a catalog target, and a retry/overlap policy — living
//! under `.orbit/routines/` in a registered owner checkout. The stateless
//! [`run_sweep_with_providers`] pass, invoked every minute by the OS clock
//! (see [`clock`]), fires whatever is due on this host through the host's
//! existing run machinery. Definitions are shared across hosts via git; all
//! scheduler state is host-local and never synced, so every owner checkout
//! is an independent schedule (design in `docs/design/routines/`).
//!
//! - [`due`] — cron parsing and catch-up eligibility.
//! - [`loader`] — discover + parse definitions, fail-closed.
//! - [`sweep`] — the deterministic fire/retry/overlap policy.
//! - [`tick`] — one host pass: lock, store, discovery, dispatch, auto-tasks.
//! - [`status`] — the read-only projection behind `orbit routine list`.
//! - [`clock`] / [`clock_unit`] — OS clock installation and inspection.

use std::path::Path;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_store::contracts::RoutineStoreBackend;

pub mod clock;
pub mod clock_unit;
pub mod due;
pub mod loader;
pub mod status;
pub mod sweep;
pub mod tick;

pub use clock::{
    ClockInstallReport, ClockSettings, ClockStatus, DEFAULT_CLOCK_CADENCE_SECONDS, clock_status,
    install_clock, load_clock_settings, save_clock_settings, set_clock_cadence, set_clock_enabled,
};
pub use clock_unit::{
    ClockUnitInspection, ClockUnitVerdict, RunningBinary, inspect_clock_unit, probe_program_version,
};
pub use due::{DueDecision, due_decision, parse_cron};
pub use loader::{
    DiscoveredWorkspaces, LoadedRoutine, RoutineCollection, RoutineLoadError, RoutineOrigin,
    RoutineWorkspaceProvider, collect_routines, collect_workspace_routines,
};
pub use status::{
    RoutineStatus, RoutineStatusReport, RoutineToggleOutcome, ScheduleDisplayState, pause_routine,
    recent_fires, resume_routine, routine_statuses_with_providers, set_routine_enabled,
};
pub use sweep::{AutoTaskSweepReport, RoutineSweepReport, SweepOptions, SweepOutcome};
pub use tick::{run_sweep_at_with_providers, run_sweep_with_providers};

/// Who this host is, as reported by routine status and sweep output. Routine
/// eligibility no longer consults it: it identifies the host whose store and
/// clock a pass acted on, for display and audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineHostIdentity {
    /// Stable machine identity.
    pub machine_id: String,
    /// Operator-facing host name.
    pub host_id: String,
}

/// Open the config-resolved machine-local scheduler store.
///
/// Scheduler state is host-local, so the database is resolved from the host
/// global root alone; no workspace layering applies.
fn open_routine_store(global_root: &Path) -> Result<Arc<dyn RoutineStoreBackend>, OrbitError> {
    let database =
        orbit_config::resolved_audit_db_path(&orbit_config::ConfigRoots::global_only(global_root))?;
    orbit_store::compose::routine_store(&database)
}

#[cfg(test)]
mod tests;
