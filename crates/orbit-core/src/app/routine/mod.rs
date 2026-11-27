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

mod toggle;

use orbit_common::OrbitError;
use orbit_common::governance::authorization::AuthorizationDenial;
use orbit_types::workflow::RoutineTrigger;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::exec::{self, App, DashboardCaller, Operation};
use crate::Core;
pub use crate::application::routines::{
    ClockStatus, RoutineEditOutcome, RoutineEditRefusal, RoutineStatusReport,
};

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
    Dashboard(DashboardCaller, DashboardWrite),
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
    Dashboard(Result<DashboardWritten, AuthorizationDenial>),
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
            Command::Dashboard(..) => Operation::dashboard_read("routine", "dashboard"),
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
            Command::Dashboard(caller, write) => {
                Outcome::Dashboard(dashboard::write(core, caller, write)?)
            }
        })
    }
}

/// Run one routine command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    match command {
        Command::Dashboard(caller, write) => {
            dashboard::write(core, caller, write).map(Outcome::Dashboard)
        }
        command => exec::run::<RoutineApp>(core, command),
    }
}
