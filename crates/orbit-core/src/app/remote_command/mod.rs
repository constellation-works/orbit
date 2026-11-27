//! The remote-command app: execute one explicit-argv command in an explicit
//! working directory (`orbit.command.exec`).

mod flow;

use orbit_common::OrbitError;
use serde_json::Value;

use super::exec::{self, App, Operation};
use crate::Core;

/// What the remote-command app can be asked to do.
pub enum Command {
    /// Execute one command as an explicit `argv` array in a confined
    /// working directory.
    Exec {
        /// Raw JSON tool input (`argv`, `working_directory`, optional
        /// `claim_token` and `timeout_ms`).
        input: Value,
        /// Optional agent override for actor attribution.
        agent: Option<String>,
        /// Optional model override for actor attribution.
        model: Option<String>,
    },
}

/// What a remote-command operation produced.
pub enum Outcome {
    /// The JSON execution result returned by `orbit.command.exec`.
    Value(Value),
}

impl Outcome {
    /// Unwrap the JSON execution result.
    pub fn into_value(self) -> Value {
        match self {
            Self::Value(value) => value,
        }
    }
}

/// Why a remote-command operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Input validation, claim check, authorization, or execution failed.
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

struct RemoteCommandApp;

impl App for RemoteCommandApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Self::Command) -> Operation {
        match command {
            Command::Exec { .. } => Operation::command("command", "exec"),
        }
    }

    fn run(core: &Core, command: Self::Command) -> Result<Self::Outcome, Self::Error> {
        match command {
            Command::Exec {
                input,
                agent,
                model,
            } => flow::exec(core.runtime(), input, agent, model)
                .map(Outcome::Value)
                .map_err(Error::from),
        }
    }
}

/// Authorize, audit and run one remote-command operation.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<RemoteCommandApp>(core, command)
}
