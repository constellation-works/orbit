//! The routine app: turn a routine on or off, edit its trigger and control
//! the host's sweep clock.
//!
//! The routine noun is not in the operation registry, so [`Command`] is
//! written by hand: the command line's `enable`/`disable` and the
//! dashboard's governed writes. Each dashboard write is decided against its
//! dashboard operation and leaves the one `dashboard.operations` row that
//! write owes.
//!
//! Routine status is assembled above this crate, from every registered
//! workspace. A command that needs it carries the read as a [`Statuses`]
//! closure, which the flow calls only once the caller is admitted.

mod dashboard;

mod edit_contract;

mod guarded_edit;

mod lease;

mod loader;

mod materialize;

mod resident_port;

mod seed;

mod status;

mod store;

mod sweep;

mod template;

mod toggle;

mod trigger_edit;

use orbit_common::OrbitError;
use orbit_types::workflow::RoutineTrigger;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::exec::{self, App, DashboardOrigin, DashboardRow, Operation};
use crate::Core;
pub use crate::application::store_opener::StoreOpener;

pub use edit_contract::{RoutineEditOutcome, RoutineEditRefusal};
pub use loader::{
    DiscoveredWorkspaces, OwnerOnlyRoutine, RetiredRoutine, RoutineLoadError,
    RoutineWorkspaceProvider, declares_any_routine_name,
};
pub(crate) use materialize::reconcile_default_routines_for_policy;
pub(crate) use orbit_automation::routines::due::{minimum_cron_interval, parse_cron};
pub(crate) use orbit_clock::doctor_clock_not_running_issue;
pub use orbit_clock::{
    ClockHealth, ClockStatus, ClockUnitConvergence, ClockUnitDrift, ClockUnitInspection,
    ClockUnitVerdict, clock_status, clock_unit_drift_warning, converge_clock_unit,
    inspect_clock_unit, install_clock, set_clock_cadence, set_clock_enabled,
};
pub(crate) use resident_port::ResidentRoutineStatus;
pub(crate) use seed::{DEFAULT_ROUTINE_FILES, RETIRED_ROUTINE_FILES};
pub use seed::{RoutineNameCollision, RoutineSeedIdentity, default_routine_name_collisions};
pub use status::{
    FireField, RoutineBreakerAction, RoutineBreakerOutcome, RoutineField, RoutineStatus,
    RoutineStatusReport, control_listed_routine_breaker, pause_routine, project_fire, recent_fires,
    resume_routine, routine_breaker_events, routine_statuses_with_providers,
};
pub(crate) use status::{
    checkout_edit_report, checkout_routine_statuses, set_routine_enabled, toggle_checkout_routine,
};
pub(crate) use store::host_routine_breakers;
pub use store::{RoutineMachineIdentity, ensure_routine_store};
pub use sweep::{
    AutoTaskSweepReport, ExternalMergeSweepReport, RoutineSweepReport, SweepContext, SweepOptions,
    SweepOutcome, run_sweep, run_sweep_pass,
};
pub(crate) use template::{ShippedShape, is_orbit_written_routine, shipped_shape_of};
pub(crate) use trigger_edit::edit_checkout_routine_trigger;
pub use trigger_edit::trigger_options;

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use orbit_automation::routines::breaker::request_breaker_bypass;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use orbit_automation::routines::sweep::record_fire_intent;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use orbit_clock::{ClockSettings, load_clock_settings, save_clock_settings};

/// Reads the host's routine status report.
pub type Statuses = Box<dyn FnOnce() -> Result<RoutineStatusReport, OrbitError> + Send>;

/// Reads the sweep clock's state and the host's routine status report.
pub type ClockObservation =
    Box<dyn FnOnce() -> Result<(ClockStatus, RoutineStatusReport), OrbitError> + Send>;

/// Stands in for the native clock change when it answers. Only a server
/// under test supplies one that does.
pub type ClockStandIn = Box<dyn FnOnce() -> Option<Result<(), OrbitError>> + Send>;

/// What the routine app can be asked to do.
pub enum Command {
    /// Set a routine's versioned `enabled` field by name, as `orbit routine
    /// enable|disable` does. Only a routine of the bound workspace changes.
    SetEnabled {
        /// The routine's name.
        name: String,
        /// The requested state.
        enabled: bool,
        /// The host's routine status read.
        statuses: Statuses,
    },
    /// One write from an authenticated dashboard session.
    Dashboard(DashboardOrigin, DashboardWrite),
}

/// The routine row a dashboard edit was sent from.
pub struct ListedRoutine {
    /// The routine's name.
    pub name: String,
    /// The workspace the row was listed under.
    pub source: String,
    /// The target the row showed.
    pub target: String,
}

/// What a clock control changes.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockAction {
    /// Start the native sweep service.
    Enable,
    /// Pause the native sweep service.
    Disable,
    /// Change the sweep cadence.
    SetCadence,
}

/// One clock control and the state the browser observed before sending it.
pub struct ClockControl {
    /// What to change.
    pub action: ClockAction,
    /// The requested cadence; required by [`ClockAction::SetCadence`].
    pub cadence_seconds: Option<u64>,
    /// Whether the browser saw the clock enabled.
    pub expected_enabled: bool,
    /// The configured cadence the browser saw.
    pub expected_cadence_seconds: u64,
    /// The global root whose clock this is.
    pub global_root: std::path::PathBuf,
    /// The host's clock and routine status read.
    pub observe: ClockObservation,
    /// A stand-in for the native change, if the host has one.
    pub stand_in: Option<ClockStandIn>,
}

/// One routine write the dashboard sends.
pub enum DashboardWrite {
    /// Set a listed routine's `enabled` field, compared with the observed one.
    Toggle {
        /// The row the edit was sent from.
        routine: ListedRoutine,
        /// The flag the browser observed.
        expected_enabled: bool,
        /// The requested state.
        enabled: bool,
        /// The host's routine status read.
        statuses: Statuses,
    },
    /// Replace a listed routine's trigger, compared with the observed one.
    Trigger {
        /// The row the edit was sent from.
        routine: ListedRoutine,
        /// The trigger the browser observed.
        expected: Box<RoutineTrigger>,
        /// The requested trigger.
        trigger: Box<RoutineTrigger>,
        /// The host's routine status read.
        statuses: Statuses,
    },
    /// A trigger edit whose body named its routine but did not decode. Only
    /// a refused session is recorded; an admitted one does nothing.
    UndecodedTrigger {
        /// The routine's name.
        name: String,
        /// The audit arguments read from the undecoded body.
        arguments: Value,
    },
    /// Control the native sweep service or its cadence.
    Clock(ClockControl),
    /// Reset or bypass a tripped routine's circuit breaker on this host.
    Breaker {
        /// The routine's name.
        name: String,
        /// The workspace the row was listed under.
        source: String,
        /// What to do with the breaker.
        action: RoutineBreakerAction,
        /// The global root whose breakers these are.
        global_root: std::path::PathBuf,
        /// Opens the host's routine store.
        opener: StoreOpener,
        /// The host's routine status read.
        statuses: Statuses,
    },
}

/// What an admitted dashboard write did.
pub enum DashboardWritten {
    /// A definition edit: what it changed, why it was refused without
    /// writing, or the error its write failed with.
    Edited(Result<Result<RoutineEditOutcome, RoutineEditRefusal>, OrbitError>),
    /// The session may edit triggers; the undecoded body was not applied.
    TriggerAdmitted,
    /// What a clock control did.
    Clock(ClockControlled),
    /// What a breaker control did, why it was refused without writing, or
    /// the error it failed with.
    Breaker(Result<Result<RoutineBreakerOutcome, RoutineEditRefusal>, OrbitError>),
}

/// What an admitted clock control did.
pub enum ClockControlled {
    /// The browser acted from another machine than this host; nothing ran.
    MachineMismatch {
        /// This host's machine name.
        machine_name: String,
    },
    /// The clock no longer matched what the browser observed; nothing ran.
    Stale(Box<ClockStatus>),
    /// The change ran from the observed state `before`.
    Applied {
        /// The clock's state before the change.
        before: Box<ClockStatus>,
        /// Whether the change succeeded.
        result: Result<(), OrbitError>,
    },
}

/// What a command produced.
pub enum Outcome {
    /// What `enable`/`disable` changed.
    EnabledSet(RoutineEditOutcome),
    /// A dashboard write: what it did, or why the session was refused.
    Dashboard(DashboardWritten),
}

/// Why a routine command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A flow refused or failed the command.
    #[error(transparent)]
    Flow(#[from] OrbitError),
}

impl From<Error> for crate::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Flow(error) => error,
        }
    }
}

struct RoutineApp;

impl App for RoutineApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        match command {
            // The operator-only `routine enable`/`routine disable` rows, and
            // the requested state like the dashboard toggle's audit row.
            Command::SetEnabled { name, enabled, .. } => {
                Operation::command("routine", if *enabled { "enable" } else { "disable" })
                    .target(Some("routine"), Some(name.clone()))
                    .arguments(&json!({ "enabled": enabled }))
            }
            Command::Dashboard(origin, write) => dashboard::operation(origin, write),
        }
    }

    fn run(core: &Core, command: Command) -> Result<Outcome, Error> {
        Ok(match command {
            Command::SetEnabled {
                name,
                enabled,
                statuses,
            } => Outcome::EnabledSet(toggle::set_enabled(
                core.runtime(),
                &name,
                enabled,
                statuses,
            )?),
            Command::Dashboard(origin, write) => {
                Outcome::Dashboard(dashboard::write(core, &origin.machine_name, write)?)
            }
        })
    }

    fn dashboard_row(
        result: &Result<Outcome, crate::Error>,
        target: &str,
        arguments: Value,
    ) -> Option<DashboardRow> {
        let written = match result {
            Ok(Outcome::Dashboard(written)) => Ok(written),
            Ok(_) => return None,
            Err(error) => Err(error),
        };
        dashboard::row(written, target, arguments)
    }
}

/// Run one routine command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<RoutineApp>(core, command)
}

#[cfg(test)]
mod tests;
