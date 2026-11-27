//! The resident app: grant, relay to, control and read resident mandates.
//!
//! [`Command::Operation`] is a verb of the resident noun in the operation
//! registry (`orbit_common::governance::resident`) with its fields, named
//! and typed as the verb's spec declares; its [`Outcome`] carries the JSON
//! that spec documents.
//!
//! A registry command from a surface still runs through the generic tool
//! entry, which checks the fields against the spec, decides the tool row and
//! reaches the resident tool handler. The handler calls [`exec`] again from
//! inside that entry, and that call runs the flow.
//!
//! The dashboard's commands have no registry verb. Its two reads answer the
//! Residents view's models; each of its writes is decided against the
//! dashboard's resident operation and leaves one `dashboard.operations` row.

mod dashboard;

mod operation;

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_common::governance::resident::ResidentVerb;
use serde_json::{Map, Value};

use super::exec::{self, App, DashboardOrigin, DashboardRow, Operation};
use super::tool_call;
pub use super::tool_call::Input;
use crate::Core;
pub use crate::application::resident::{
    ResidentDashboardGrant, ResidentDashboardGrantOutcome, ResidentDashboardGrantParams,
};

/// What the resident app can be asked to do.
pub enum Command {
    /// One registry verb and the fields it is called with.
    Operation(ResidentVerb, Input),
    /// Read every mandate as a card, with what the Residents view shows
    /// beside them.
    Cards {
        /// Include ended mandates.
        include_ended: bool,
    },
    /// Read one mandate as a card, for a session that is an operator's or
    /// not.
    Card {
        /// The mandate's assignment.
        task_id: String,
        /// Whether the session holds operator capability.
        operator: bool,
    },
    /// One write from an authenticated dashboard session.
    Dashboard(DashboardOrigin, DashboardWrite),
}

/// One resident write the dashboard sends.
pub enum DashboardWrite {
    /// Create a mandate's assignment and artifact.
    Create {
        /// Assignment title.
        title: String,
        /// Assignment narrative.
        description: String,
        /// Assignment complexity as the form names it; `medium` when absent.
        complexity: Option<String>,
        /// The `resident.mandate/v1` draft.
        mandate: Value,
    },
    /// Narrow, widen as an answered request, or both.
    Grant(ResidentDashboardGrantParams),
    /// The manager channel's control, as the resident control tool takes it.
    Control(Map<String, Value>),
    /// The operator's own words, as the resident relay tool takes them.
    Relay(Map<String, Value>),
}

/// The Residents view's list model.
pub struct Cards {
    /// The workspace's display label.
    pub workspace: String,
    /// One card per mandate.
    pub mandates: Vec<Value>,
    /// The configured crew registry.
    pub crews: Value,
    /// Where the workspace's resident cycle routine is defined, when the
    /// workspace has an identity.
    pub cycle_routine: Option<CycleRoutineScope>,
}

/// The workspace whose routine definitions hold its resident cycle routine.
pub struct CycleRoutineScope {
    /// The logical catalog id the dashboard addressed.
    pub workspace_id: String,
    /// The `.orbit` directory the routine is loaded from.
    pub orbit_dir: PathBuf,
}

/// What an admitted dashboard write answered.
pub enum DashboardWritten {
    /// The JSON a create, control or relay answered with.
    Value(Value),
    /// What a grant form did.
    Granted(Box<ResidentDashboardGrant>),
}

/// What a command produced.
pub enum Outcome {
    /// The JSON a registry verb's spec documents, or one card.
    Value(Value),
    /// The list model.
    Cards(Box<Cards>),
    /// A dashboard write: what it answered, or why the session was refused.
    Dashboard(DashboardWritten),
}

impl Outcome {
    /// The JSON a registry verb or card read answered with; null otherwise.
    pub fn into_value(self) -> Value {
        match self {
            Self::Value(value) => value,
            Self::Cards(_) | Self::Dashboard(_) => Value::Null,
        }
    }
}

/// Why a resident command failed.
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

struct ResidentApp;

impl App for ResidentApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        match command {
            // The generic tool entry decides the tool row.
            Command::Operation(verb, input) => {
                Operation::of(*verb, &input.fields).without_tool_row()
            }
            Command::Cards { .. } => Operation::dashboard_read("resident", "cards"),
            Command::Card { .. } => Operation::dashboard_read("resident", "card"),
            Command::Dashboard(origin, write) => dashboard::operation(origin, write),
        }
    }

    fn run(core: &Core, command: Command) -> Result<Outcome, Error> {
        Ok(match command {
            Command::Operation(verb, input) => Outcome::Value(tool_call::run(
                core,
                verb,
                input,
                |runtime, input, caller| operation::dispatch(runtime, verb, input, caller),
            )?),
            Command::Cards { include_ended } => {
                Outcome::Cards(Box::new(dashboard::cards(core.runtime(), include_ended)?))
            }
            Command::Card { task_id, operator } => {
                Outcome::Value(dashboard::card(core, task_id, operator)?)
            }
            Command::Dashboard(_, write) => Outcome::Dashboard(dashboard::write(core, write)?),
        })
    }

    fn dashboard_row(
        result: &Result<Outcome, crate::Error>,
        _target: &str,
        arguments: Value,
    ) -> Option<DashboardRow> {
        let written = match result {
            Ok(Outcome::Dashboard(written)) => Ok(written),
            Ok(_) => return None,
            Err(error) => Err(error),
        };
        dashboard::row(written, arguments)
    }
}

/// Run one resident command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<ResidentApp>(core, command)
}
