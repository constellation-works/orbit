//! The automation app: explain a delivery automation, clear a stalled
//! condition, and read a consumer's persisted state and coverage evidence.
//!
//! [`Command::Operation`] is a verb of the automation noun in the operation
//! registry (`orbit_common::governance::automation`) with its fields, named
//! and typed as the verb's spec declares; its [`Outcome`] carries the JSON
//! that spec documents.
//!
//! A registry command from a surface still runs through the generic tool
//! entry, which checks the fields against the spec, decides the tool row and
//! reaches the automation tool handler. The handler calls [`exec`] again from
//! inside that entry, and that call runs the flow. The two dashboard reads
//! have no registry verb and no tool: they read the store directly.

mod operation;

mod read;

use orbit_common::OrbitError;
use orbit_common::governance::automation::AutomationVerb;
use serde_json::Value;

use super::exec::{self, App, Operation};
use super::tool_call;
pub use super::tool_call::Input;
use crate::Core;

/// What the automation app can be asked to do.
pub enum Command {
    /// One registry verb and the fields it is called with.
    Operation(AutomationVerb, Input),
    /// Read the persisted membership state of one delivery consumer.
    State(Consumer),
    /// Read the evidence one consumer accepted for `batch`.
    Evidence {
        /// The consumer that accepted the batch.
        consumer: Consumer,
        /// The accepted batch's id.
        batch: String,
    },
}

/// One delivery consumer of this workspace on this machine.
pub struct Consumer {
    /// `auto-task` or `routine`.
    pub kind: String,
    /// The definition's name.
    pub name: String,
}

/// What a command produced.
pub enum Outcome {
    /// The JSON a registry verb's spec documents.
    Value(Value),
    /// The consumer's persisted state, if it has any.
    State(Option<Value>),
    /// The accepted evidence document, if the batch has a receipt.
    Evidence(Option<Vec<u8>>),
}

impl Outcome {
    /// The JSON a registry verb answered with; null for a dashboard read.
    pub fn into_value(self) -> Value {
        match self {
            Self::Value(value) => value,
            Self::State(_) | Self::Evidence(_) => Value::Null,
        }
    }
}

/// Why an automation command failed.
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

struct AutomationApp;

impl App for AutomationApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        match command {
            // The generic tool entry decides the tool row.
            Command::Operation(verb, input) => {
                Operation::of(*verb, &input.fields).without_tool_row()
            }
            Command::State(_) => Operation::dashboard_read("automation", "state"),
            Command::Evidence { .. } => Operation::dashboard_read("automation", "evidence"),
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
            Command::State(consumer) => Outcome::State(read::state(core.runtime(), &consumer)?),
            Command::Evidence { consumer, batch } => {
                Outcome::Evidence(read::evidence(core.runtime(), &consumer, &batch)?)
            }
        })
    }
}

/// Run one automation command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<AutomationApp>(core, command)
}
