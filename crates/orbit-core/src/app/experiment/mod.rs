//! The experiment app: define, run and read experiments.
//!
//! [`Command`] is a verb of the experiment noun in the operation registry
//! (`orbit_common::governance::experiment`) with its fields, named and typed as
//! the verb's spec declares; its [`Outcome`] carries the JSON that spec
//! documents.
//!
//! A command from a surface still runs through the generic tool entry, which
//! checks the fields against the spec, decides the tool row and reaches the
//! experiment tool handler. The handler calls [`exec`] again from inside that
//! entry, and that call runs the flow.

mod operation;

use orbit_common::OrbitError;
use orbit_common::governance::experiment::ExperimentVerb;
use serde_json::Value;

use super::exec::{self, App, Operation};
use super::tool_call;
pub use super::tool_call::Input;
use crate::Core;

/// One experiment verb and the fields it is called with.
pub struct Command {
    verb: ExperimentVerb,
    input: Input,
}

impl Command {
    /// The command for `verb`, called with `input`.
    pub fn new(verb: ExperimentVerb, input: Input) -> Self {
        Self { verb, input }
    }
}

/// What a command produced: the JSON its verb's spec documents.
pub struct Outcome(Value);

impl Outcome {
    /// The JSON this outcome carries.
    pub fn into_value(self) -> Value {
        self.0
    }
}

/// Why an experiment command failed.
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

struct ExperimentApp;

impl App for ExperimentApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        // The generic tool entry decides the tool row.
        Operation::of(command.verb, &command.input.fields).without_tool_row()
    }

    fn run(core: &Core, command: Command) -> Result<Outcome, Error> {
        let Command { verb, input } = command;
        let value = tool_call::run(core, verb, input, |runtime, input, caller| {
            operation::dispatch(
                runtime,
                verb,
                input,
                caller.agent.as_deref(),
                caller.model.as_deref(),
            )
        })?;
        Ok(Outcome(value))
    }
}

/// Run one experiment command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<ExperimentApp>(core, command)
}
