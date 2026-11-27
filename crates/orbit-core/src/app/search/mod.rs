//! The search app: query tasks, friction and docs, and rebuild the task
//! search index.
//!
//! [`Command::Operation`] holds the query verb of the search noun in the operation
//! registry (`orbit_common::governance::search`) with its fields, named and
//! typed as the verb's spec declares; its [`Outcome::Value`] carries the JSON
//! that spec documents. Such a command from a surface still runs through the
//! generic tool entry, which checks the fields against the spec, decides the
//! tool row and reaches the search tool handler. The handler calls [`exec`]
//! again from inside that entry, and that call runs the flow.
//!
//! Reindexing is local maintenance outside the registry.

mod operation;

use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationVerb;
use orbit_common::governance::search::SearchVerb;
use orbit_search::SearchIndexStats;
use serde_json::Value;

use super::exec::{self, App, Operation};
use super::tool_call;
pub use super::tool_call::Input;
use crate::Core;

// Surface re-exports: what the CLI, dashboard and composition name from this subject.
pub use crate::application::search::{
    GlobalSearchKind, GlobalSearchParams, SearchRequest, SearchRequestProfile,
};

/// What the search app can be asked to do.
pub enum Command {
    /// A registry verb of the search noun and the fields it is called with.
    Operation(SearchVerb, Input),
    /// Rebuild the task search index from the task store. The fields are the
    /// query flags the invocation carried, which name its audit row.
    Reindex(Input),
}

/// What a command produced.
pub enum Outcome {
    /// The JSON a registry verb's spec documents.
    Value(Value),
    /// What the rebuilt index holds.
    Reindexed(SearchIndexStats),
}

impl Outcome {
    /// The JSON this outcome carries.
    pub fn into_value(self) -> Result<Value, crate::Error> {
        match self {
            Self::Value(value) => Ok(value),
            Self::Reindexed(stats) => serde_json::to_value(stats)
                .map_err(|error| OrbitError::Execution(error.to_string())),
        }
    }
}

/// Why a search command failed.
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

struct SearchApp;

impl App for SearchApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        match command {
            // The generic tool entry decides the tool row.
            Command::Operation(verb, input) => Operation::of(*verb, &input.fields)
                .without_tool_row()
                .subcommand(audit_subcommand(&verb.spec().cli_path.join("-"), input)),
            Command::Reindex(input) => Operation::of(SearchVerb::Query, &input.fields)
                .without_tool_row()
                .subcommand(audit_subcommand("reindex", input)),
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
            Command::Reindex(_) => Outcome::Reindexed(core.runtime().search_reindex()?),
        })
    }
}

/// Run one search command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<SearchApp>(core, command)
}

/// The audit label search has always been recorded under: the mode, then the
/// searched kind.
fn audit_subcommand(mode: &str, input: &Input) -> String {
    let kind = input
        .fields
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("all");
    format!("{mode}:{kind}")
}
