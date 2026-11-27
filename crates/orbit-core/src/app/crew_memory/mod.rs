//! The crew memory app: a run's read, write and proposal of its crew's
//! memory.
//!
//! [`Command`] is a verb of the memory noun in the operation registry
//! (`orbit_common::governance::memory`) with its fields, named and typed as
//! the verb's spec declares; its [`Outcome`] carries the JSON that spec
//! documents. A command from a surface still runs through the generic tool
//! entry, which checks the fields against the spec, decides the tool row and
//! reaches the memory tool handler. The handler calls [`exec`] again from
//! inside that entry, and that call runs the flow with the caller's trusted
//! run binding.

mod operation;

use orbit_common::OrbitError;
use orbit_common::governance::memory::MemoryVerb;
use serde_json::Value;

use super::exec::{self, App, Operation};
use super::tool_call;
pub use super::tool_call::Input;
use crate::Core;

/// One memory verb and the fields it is called with.
pub struct Command {
    verb: MemoryVerb,
    input: Input,
}

impl Command {
    /// The command for `verb`, called with `input`.
    pub fn new(verb: MemoryVerb, input: Input) -> Self {
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

/// Why a crew memory command failed.
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

struct CrewMemoryApp;

impl App for CrewMemoryApp {
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
            operation::dispatch(runtime, verb, input, caller)
        })?;
        Ok(Outcome(value))
    }
}

/// Run one crew memory command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<CrewMemoryApp>(core, command)
}
