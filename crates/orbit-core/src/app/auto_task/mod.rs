//! The auto-task app: define, list, toggle, mint and recover auto-tasks.
//!
//! [`Command::Operation`] is a verb of the auto-task noun in the operation
//! registry (`orbit_common::governance::auto_task`) with its fields, named
//! and typed as the verb's spec declares; its [`Outcome`] carries the JSON
//! that spec documents.
//!
//! A registry command from a surface still runs through the generic tool
//! entry, which checks the fields against the spec, decides the tool row and
//! reaches the auto-task tool handler. The handler calls [`exec`] again from
//! inside that entry, and that call runs the flow.
//!
//! The dashboard's commands have no registry verb. Its listing read answers
//! the Auto-tasks panel's model; each of its writes is decided against its
//! dashboard operation and leaves the one `dashboard.operations` row that
//! write owes.

mod dashboard;
mod operation;

use orbit_common::OrbitError;
use orbit_common::governance::authorization::AuthorizationDenial;
use orbit_common::governance::auto_task::AutoTaskVerb;
use orbit_types::workflow::AutoTaskSchedule;
use serde_json::Value;

use super::exec::{self, App, DashboardCaller, Operation};
use super::tool_call;
pub use super::tool_call::Input;
use crate::application::auto_tasks::delete::AutoTaskDeleteReport;
pub use crate::application::auto_tasks::delete::{
    AutoTaskDeleteProfile, project_auto_task_delete_receipt,
};
pub use crate::application::auto_tasks::loader::AutoTaskLoadError;
pub use crate::application::auto_tasks::projection::AutoTaskListingProfile;
pub use crate::application::auto_tasks::{
    AutoTaskDeleteError, AutoTaskDeleteParams, AutoTaskDeleteRefusal, AutoTaskScheduleError,
    AutoTaskScheduleOutcome, AutoTaskToggleError, AutoTaskToggleOutcome,
};
use crate::{Core, Task};

/// What the auto-task app can be asked to do.
pub enum Command {
    /// One registry verb and the fields it is called with.
    Operation(AutoTaskVerb, Input),
    /// Read the Auto-tasks panel's listing.
    Listing {
        /// Also list definitions whose seeding plugin is off here.
        include_inactive_plugins: bool,
    },
    /// One write from an authenticated dashboard session.
    Dashboard(DashboardCaller, DashboardWrite),
}

/// One auto-task write the dashboard sends.
pub enum DashboardWrite {
    /// Create a definition from the structured add fields.
    Create {
        /// The fields auto-task add accepts.
        fields: Value,
        /// The definition's initial enabled state.
        enabled: Option<bool>,
    },
    /// Set a definition's `enabled` flag, compared with the observed one.
    Toggle {
        /// The definition's name.
        name: String,
        /// The raw flag the browser observed.
        expected_enabled: bool,
        /// The requested state.
        enabled: bool,
    },
    /// Mint one task from a definition, whatever its schedule says.
    Mint {
        /// The definition's name.
        name: String,
    },
    /// Replace a definition's schedule, compared with the observed one.
    Schedule {
        /// The definition's name.
        name: String,
        /// The schedule the browser observed.
        expected_schedule: Box<AutoTaskSchedule>,
        /// The requested schedule.
        schedule: Box<AutoTaskSchedule>,
    },
    /// A schedule edit whose body named its definition but did not decode.
    /// Only a refused session is recorded; an admitted one does nothing.
    UndecodedSchedule {
        /// The definition's name.
        name: String,
        /// The audit arguments read from the undecoded body.
        arguments: Value,
    },
    /// Remove a definition, its cursor and its delivery-consumer state.
    Delete(AutoTaskDeleteParams),
}

/// The Auto-tasks panel's listing.
pub struct Listing {
    /// One row per listed definition.
    pub definitions: Vec<Value>,
    /// Definitions whose seeding plugin is off here, listed or not.
    pub inactive_plugin_count: usize,
    /// Why the host cursor state could not be read.
    pub cursor_state_error: Option<String>,
    /// Definition files that failed to load.
    pub load_errors: Vec<AutoTaskLoadError>,
    /// Why this workspace's role cannot write tasks, when it cannot.
    pub role_refusal: Option<String>,
}

/// What an admitted dashboard write did. Each carries the result the
/// runtime's own command answered with.
pub enum DashboardWritten {
    /// The created definition.
    Created(Result<Value, OrbitError>),
    /// What the toggle did.
    Toggled(Result<AutoTaskToggleOutcome, AutoTaskToggleError>),
    /// The minted task.
    Minted(Result<Box<Task>, OrbitError>),
    /// What the schedule edit did.
    Scheduled(Result<AutoTaskScheduleOutcome, AutoTaskScheduleError>),
    /// The session may edit schedules; the undecoded body was not applied.
    ScheduleAdmitted,
    /// What the delete removed.
    Deleted(Result<Box<AutoTaskDeleteReport>, AutoTaskDeleteError>),
}

/// What a command produced.
pub enum Outcome {
    /// The JSON a registry verb's spec documents.
    Value(Value),
    /// The listing.
    Listing(Box<Listing>),
    /// A dashboard write: what it did, or why the session was refused.
    Dashboard(Box<Result<DashboardWritten, AuthorizationDenial>>),
}

impl Outcome {
    /// The JSON a registry verb answered with; null otherwise.
    pub fn into_value(self) -> Value {
        match self {
            Self::Value(value) => value,
            Self::Listing(_) | Self::Dashboard(_) => Value::Null,
        }
    }
}

/// Why an auto-task command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A flow, or the tool entry in front of it, refused or failed the
    /// command.
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

struct AutoTaskApp;

impl App for AutoTaskApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        match command {
            // The generic tool entry decides the tool row.
            Command::Operation(verb, input) => {
                Operation::of(*verb, &input.fields).without_tool_row()
            }
            Command::Listing { .. } | Command::Dashboard(..) => {
                Operation::dashboard_read("auto-task", "listing")
            }
        }
    }

    fn run(core: &Core, command: Command) -> Result<Outcome, Error> {
        Ok(match command {
            Command::Operation(verb, input) => Outcome::Value(tool_call::run(
                core,
                verb,
                input,
                |runtime, input, _caller| operation::dispatch(runtime, verb, input),
            )?),
            Command::Listing {
                include_inactive_plugins,
            } => Outcome::Listing(Box::new(dashboard::listing(
                core.runtime(),
                include_inactive_plugins,
            )?)),
            Command::Dashboard(caller, write) => {
                Outcome::Dashboard(Box::new(dashboard::write(core, caller, write)?))
            }
        })
    }
}

/// Run one auto-task command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    match command {
        Command::Dashboard(caller, write) => dashboard::write(core, caller, write)
            .map(|written| Outcome::Dashboard(Box::new(written))),
        command => exec::run::<AutoTaskApp>(core, command),
    }
}
