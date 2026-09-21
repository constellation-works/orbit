//! Routines [ORB-10021]: Orbit as the constellation's single scheduler.
//!
//! A routine is a durable YAML definition of recurring work — a cron trigger,
//! a catalog target, and a retry/overlap policy — living under
//! `.orbit/routines/` in a registered owner checkout. That directory is
//! per-user state, not a repository artifact. The stateless
//! [`run_sweep_with_providers`] pass, invoked every minute by the OS clock
//! (see [`clock`]), fires whatever is due on this host through the existing
//! v2 run machinery. Every owner checkout is an independent schedule
//! (design in `docs/design/routines/`).

use std::path::Path;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_store::contracts::RoutineStoreBackend;

pub mod clock;
pub mod clock_unit;
pub use orbit_automation::routines::due;
pub mod loader;
pub mod status;
pub mod sweep;

pub use clock::{
    ClockInstallReport, ClockSettings, ClockStatus, DEFAULT_CLOCK_CADENCE_SECONDS, clock_status,
    install_clock, load_clock_settings, save_clock_settings, set_clock_cadence, set_clock_enabled,
};
pub use clock_unit::{
    ClockUnitConvergence, ClockUnitDrift, ClockUnitInspection, ClockUnitReload, ClockUnitRewrite,
    ClockUnitVerdict, RunningBinary, clock_unit_drift_warning, converge_clock_unit,
    inspect_clock_unit, probe_program_version,
};
pub use due::{DueDecision, due_decision, parse_cron};
pub use loader::{
    DiscoveredWorkspaces, LoadedRoutine, RoutineCollection, RoutineLoadError, RoutineOrigin,
    RoutineWorkspaceProvider, collect_routines,
};
pub(crate) use status::rewrite_enabled_line;
pub use status::{
    RoutineStatus, RoutineStatusReport, RoutineToggleOutcome, ScheduleDisplayState, pause_routine,
    recent_fires, resume_routine, routine_statuses_with_providers, set_routine_enabled,
};
pub use sweep::{
    AutoTaskSweepReport, RoutineSweepReport, SweepOptions, SweepOutcome,
    run_sweep_at_with_providers, run_sweep_with_providers,
};

/// Who this machine is, as reported by routine status and sweep output.
/// Routine eligibility no longer consults it: it identifies the machine whose
/// store and clock a pass acted on, for display and audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineMachineIdentity {
    /// Stable machine identity (`machine.id`).
    pub machine_id: String,
    /// Operator-facing machine name (`machine.name`).
    pub machine_name: String,
}

/// Open the config-resolved machine-local scheduler store.
fn open_routine_store(global_root: &Path) -> Result<Arc<dyn RoutineStoreBackend>, OrbitError> {
    let database =
        orbit_config::resolved_audit_db_path(&orbit_config::ConfigRoots::global_only(global_root))?;
    orbit_store::compose::routine_store(&database)
}

#[cfg(test)]
mod tests;
