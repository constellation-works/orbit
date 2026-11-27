//! The agent app: the agent scorecards, the model ranking, the vision probe
//! and the recorded CI failures.
//!
//! [`Command::Operation`] is a verb of the agents noun in the operation
//! registry (`orbit_common::governance::agents`) with its fields, named and
//! typed as the verb's spec declares; its [`Outcome::Value`] carries the JSON
//! that spec documents. Such a command from a surface still runs through the
//! generic tool entry, which checks the fields against the spec, decides the
//! tool row and reaches the agents tool handler. The handler calls [`exec`]
//! again from inside that entry, and that call runs the flow.
//!
//! The ranking, its calibration and the vision probe are hand-written
//! commands outside the registry. The scan and the facts the dashboard slices
//! from it are read models: their [`Outcome`] is the model.

mod operation;

use orbit_common::OrbitError;
use orbit_common::governance::agents::AgentsVerb;
use orbit_config::TaskTypeDefinition;
use orbit_types::record::CrewDiscoveryV1;
use serde_json::Value;

use super::exec::{self, App, Operation};
use super::tool_call;
pub use super::tool_call::Input;
use crate::Core;
use crate::application::agents::{AgentFacts, CiFailureQuery, RankQuery, RankSeasons};
use crate::application::vision_probe::VisionProbeReport;

/// What the agent app can be asked to do.
pub enum Command {
    /// A registry verb of the agents noun and the fields it is called with.
    Operation(AgentsVerb, Input),
    /// Rank implementer models for a selection.
    Rank(RankQuery),
    /// Calibrate the ranking's noise scales and record a new season.
    RankCalibrate(RankQuery),
    /// Run the image canary once per named crew, or per configured crew.
    VisionProbe {
        /// The crews to probe; empty probes every crew.
        crews: Vec<String>,
        /// How long one canary may run.
        timeout_seconds: u64,
    },
    /// The dashboard's read of the workspace's agent scan.
    Scan,
    /// The dashboard's read of the workspace's configured task types.
    TaskTypes,
    /// The dashboard's read of the workspace's ranking seasons.
    RankSeasons,
    /// The dashboard's read of the recorded required-check failures.
    CiFailures(CiFailureQuery),
    /// Discover effective crews for a workspace, for MCP workspace discovery.
    CrewDiscovery {
        /// Registered logical workspace id.
        workspace_id: String,
        /// Registered owner machine id, if any.
        owner_machine_id: Option<String>,
    },
}

/// What a command produced.
pub enum Outcome {
    /// The JSON a registry verb, the ranking, its calibration or the CI
    /// failure read documents.
    Value(Value),
    /// The vision probe's report.
    VisionProbe(VisionProbeReport),
    /// The agent scan.
    Scan(Box<AgentFacts>),
    /// The configured task types.
    TaskTypes(Vec<TaskTypeDefinition>),
    /// The ranking seasons on file.
    RankSeasons(RankSeasons),
    /// Effective crew discovery for one workspace.
    CrewDiscovery(CrewDiscoveryV1),
}

impl Outcome {
    /// The JSON this outcome carries, for a command that answers JSON.
    pub fn into_value(self) -> Result<Value, crate::Error> {
        match self {
            Self::Value(value) => Ok(value),
            Self::VisionProbe(report) => serde_json::to_value(&report)
                .map_err(|error| OrbitError::Execution(error.to_string())),
            Self::CrewDiscovery(discovery) => serde_json::to_value(discovery).map_err(|error| {
                OrbitError::Execution(format!("serialize crew discovery: {error}"))
            }),
            Self::Scan(_) | Self::TaskTypes(_) | Self::RankSeasons(_) => Err(
                OrbitError::Execution("agent read model has no JSON document".to_string()),
            ),
        }
    }
}

/// Why an agent command failed.
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

struct AgentApp;

impl App for AgentApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        match command {
            // The generic tool entry decides the tool row.
            Command::Operation(verb, input) => {
                Operation::of(*verb, &input.fields).without_tool_row()
            }
            // The ranking reads the scan and writes no invocation row.
            Command::Rank(_) => Operation::command("agents", "rank")
                .without_command_row()
                .without_invocation_audit(),
            // Calibration writes the workspace's season file.
            Command::RankCalibrate(_) => Operation::command("agents", "rank-calibrate")
                .target(Some("rank_season"), None)
                .without_command_row(),
            // The probe submits one canary run per crew.
            Command::VisionProbe { .. } => Operation::command("agents", "vision-probe")
                .target(Some("crew"), None)
                .without_command_row(),
            Command::Scan => Operation::dashboard_read("agents", "scan"),
            Command::TaskTypes => Operation::dashboard_read("agents", "task-types"),
            Command::RankSeasons => Operation::dashboard_read("agents", "rank-seasons"),
            Command::CiFailures(_) => Operation::dashboard_read("agents", "ci-failures"),
            Command::CrewDiscovery { .. } => Operation::dashboard_read("agents", "crew-discovery"),
        }
    }

    fn run(core: &Core, command: Command) -> Result<Outcome, Error> {
        let runtime = core.runtime();
        Ok(match command {
            Command::Operation(verb, input) => Outcome::Value(tool_call::run(
                core,
                verb,
                input,
                |runtime, input, caller| operation::dispatch(runtime, verb, input, caller),
            )?),
            Command::Rank(query) => Outcome::Value(runtime.agent_rank(&query)?),
            Command::RankCalibrate(query) => Outcome::Value(runtime.agent_rank_calibrate(&query)?),
            Command::VisionProbe {
                crews,
                timeout_seconds,
            } => Outcome::VisionProbe(runtime.vision_probe(&crews, timeout_seconds)?),
            Command::Scan => Outcome::Scan(Box::new(runtime.agent_scan()?)),
            Command::TaskTypes => Outcome::TaskTypes(runtime.task_types().to_vec()),
            Command::RankSeasons => Outcome::RankSeasons(runtime.agent_rank_seasons()?),
            Command::CiFailures(query) => Outcome::Value(runtime.ci_failure_records(&query)?),
            Command::CrewDiscovery {
                workspace_id,
                owner_machine_id,
            } => Outcome::CrewDiscovery(runtime.crew_discovery(&workspace_id, owner_machine_id)?),
        })
    }
}

/// Run one agent command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<AgentApp>(core, command)
}
