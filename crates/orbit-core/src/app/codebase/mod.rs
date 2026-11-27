//! The codebase app: which repository files are expensive or risky to
//! change, one path's standing, and which files change alongside them.
//!
//! [`Command::Operation`] is a verb of the codebase noun in the operation
//! registry (`orbit_common::governance::codebase`) with its fields, named and
//! typed as the verb's spec declares; its [`Outcome::Value`] carries the JSON
//! that spec documents. Such a command from a surface still runs through the
//! generic tool entry, which checks the fields against the spec, decides the
//! tool row and reaches the codebase tool handler. The handler calls [`exec`]
//! again from inside that entry, and that call runs the flow.
//!
//! The scans and the regression layers the dashboard slices and checks its
//! selection against are read models: their [`Outcome`] is the model.

mod operation;

use orbit_common::OrbitError;
use orbit_common::governance::codebase::CodebaseVerb;
use orbit_config::RegressionLayerDefinition;
use serde_json::Value;

use super::exec::{self, App, Operation};
use super::tool_call;
pub use super::tool_call::Input;
use crate::Core;

// Surface re-exports: what the CLI, dashboard and composition name from this subject.
pub use crate::application::codebase::{
    CodebaseFacts, CodebaseQuery, DIFF_STATS_BACKFILL_DEFAULT_LIMIT, DiffStatsBackfill,
    RegressionFilter, RelatedQuery, TaskRiskinessModel, codebase_analytics, codebase_path_detail,
    codebase_related,
};

/// What the codebase app can be asked to do.
pub enum Command {
    /// A registry verb of the codebase noun and the fields it is called with.
    Operation(CodebaseVerb, Input),
    /// The dashboard's read of the workspace's history-only scan.
    Scan,
    /// The dashboard's read of the related-file scan for a selection's window.
    RelatedScan(CodebaseQuery),
    /// The dashboard's read of the workspace's `workflow.regression_layers`.
    RegressionLayers,
}

/// What a command produced.
pub enum Outcome {
    /// The JSON a registry verb's spec documents.
    Value(Value),
    /// A codebase scan.
    Facts(CodebaseFacts),
    /// The configured regression layers.
    RegressionLayers(Vec<RegressionLayerDefinition>),
}

impl Outcome {
    /// The JSON this outcome carries, for a command that answers JSON.
    pub fn into_value(self) -> Result<Value, crate::Error> {
        match self {
            Self::Value(value) => Ok(value),
            Self::Facts(_) | Self::RegressionLayers(_) => Err(OrbitError::Execution(
                "codebase read model has no JSON document".to_string(),
            )),
        }
    }
}

/// Why a codebase command failed.
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

struct CodebaseApp;

impl App for CodebaseApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        match command {
            // The generic tool entry decides the tool row.
            Command::Operation(verb, input) => {
                Operation::of(*verb, &input.fields).without_tool_row()
            }
            Command::Scan => Operation::dashboard_read("codebase", "scan"),
            Command::RelatedScan(_) => Operation::dashboard_read("codebase", "related-scan"),
            Command::RegressionLayers => Operation::dashboard_read("codebase", "regression-layers"),
        }
    }

    fn run(core: &Core, command: Command) -> Result<Outcome, Error> {
        let runtime = core.runtime();
        Ok(match command {
            Command::Operation(verb, input) => Outcome::Value(tool_call::run(
                core,
                verb,
                input,
                |runtime, input, _caller| operation::dispatch(runtime, verb, input),
            )?),
            Command::Scan => Outcome::Facts(runtime.codebase_scan()?),
            Command::RelatedScan(query) => Outcome::Facts(runtime.codebase_related_scan(&query)?),
            Command::RegressionLayers => {
                Outcome::RegressionLayers(runtime.regression_layers().to_vec())
            }
        })
    }
}

/// Run one codebase command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<CodebaseApp>(core, command)
}
