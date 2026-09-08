//! Routines [ORB-10021]: Orbit as the constellation's single scheduler.
//!
//! A routine is a durable, git-versioned YAML definition of recurring work —
//! a cron trigger, a catalog target, host pinning, and a retry/overlap
//! policy — living under `.orbit/routines/` in a routine-source workspace
//! (`[routines] role = "source"`). The stateless [`run_sweep`] pass, invoked
//! every minute by the OS clock (see [`clock`]), fires whatever is due on
//! the current host through the existing v2 run machinery. Definitions are
//! shared across hosts via git; all scheduler state is host-local and never
//! synced (ADR-0204..ADR-0208; design in `docs/design/routines/`).

use std::path::Path;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_store::contracts::RoutineStoreBackend;

pub mod clock;
pub use orbit_automation::routines::due;
pub mod loader;
pub mod status;
pub mod sweep;
pub use orbit_automation::routines::validation;

pub use clock::{
    ClockInstallReport, ClockSettings, ClockStatus, DEFAULT_CLOCK_CADENCE_SECONDS, clock_status,
    install_clock, load_clock_settings, save_clock_settings, set_clock_cadence, set_clock_enabled,
};
pub use due::{DueDecision, due_decision, parse_cron};
pub use loader::{
    DiscoveredWorkspaces, LoadedRoutine, RoutineCollection, RoutineLoadError, RoutineOrigin,
    RoutineWorkspaceProvider, collect_routines,
};
pub use status::{
    RoutineStatus, RoutineStatusReport, RoutineToggleOutcome, ScheduleDisplayState, pause_routine,
    recent_fires, resume_routine, routine_statuses_with_providers, set_routine_enabled,
};
pub use sweep::{
    RoutineSweepReport, SweepOptions, SweepOutcome, run_sweep_at_with_providers,
    run_sweep_with_providers,
};
pub use validation::{
    RoutineDiagnosticSeverity, RoutineHostIdentity, RoutineHostIdentityView, RoutinePinValidation,
    RoutinePlacementProjection, RoutinePlacementProvider, RoutineRegistryStatus,
    RoutineRegistryView, RoutineValidationDiagnostic, validate_routine_pins,
};

/// Open the config-resolved machine-local scheduler store.
fn open_routine_store(global_root: &Path) -> Result<Arc<dyn RoutineStoreBackend>, OrbitError> {
    let database =
        orbit_config::resolved_audit_db_path(&orbit_config::ConfigRoots::global_only(global_root))?;
    orbit_store::compose::routine_store(&database)
}

#[cfg(test)]
mod tests;
