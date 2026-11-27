//! The agent app: the agent scorecards, the model ranking, the vision probe,
//! the agent corpus and the recorded CI failures.
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

mod api;

mod attribution;

mod candidate;

mod ci_failure;

mod corpus_outbox;

mod crew_name;

mod effort;

mod effort_backfill;

mod fact;

mod hand;

mod memory_evidence;

mod operation;

mod query;

mod rank;

mod record;

mod role;

mod score;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::governance::agents::AgentsVerb;
use orbit_config::TaskTypeDefinition;
use orbit_types::record::CrewDiscoveryV1;
use serde_json::Value;

use super::exec::{self, App, Operation};
use super::tool_call;
pub use super::tool_call::Input;
use crate::Core;
use crate::application::vision_probe::VisionProbeReport;

// Surface re-exports: what the CLI, dashboard and composition name from this subject.
pub use api::{
    agent_attribution, agent_by_type_scorecard, agent_detail_with_effort, agent_scorecard,
    agent_size_curves,
};
pub use ci_failure::CiFailureQuery;
pub use effort_backfill::{ClaimedEffortBackfill, DispatchEffortBackfill};
pub use fact::AgentFacts;
pub use rank::{RankQuery, RankSeasons, agent_rank};

// Crate-internal re-exports for sibling apps, units and adapters.
#[cfg(not(feature = "test-support"))]
pub(crate) use api::agent_scan;
#[cfg(not(feature = "test-support"))]
use attribution::Owner;
pub(crate) use corpus_outbox::{TaskPort, agent_corpus_for_activity, read_agent_corpus_at};
pub(crate) use effort::leaf_reported_effort;
#[cfg(not(feature = "test-support"))]
pub(crate) use effort_backfill::backfill_claimed_leaf_effort;
pub(crate) use effort_backfill::backfill_dispatch_effort;
#[cfg(not(feature = "test-support"))]
pub(crate) use fact::ExecutionMode;
#[cfg(not(feature = "test-support"))]
use fact::Problem;
pub(crate) use fact::{review_gate_verdicts, task_execution_modes};
#[cfg(not(feature = "test-support"))]
pub(crate) use hand::task_handled_by;
pub(crate) use hand::task_recorded_crews;
pub(crate) use memory_evidence::{
    EvidenceSection, ModelEvidence, implementer_units_by_model, model_evidence,
};
use query::WindowBounds;

// Integration-fixture re-exports behind the dev-only `test-support` feature.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use api::{agent_detail, agent_role_scorecard, agent_scan, per_task_export};
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use attribution::Owner;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use effort_backfill::{ClaimedEffortOutcome, backfill_claimed_leaf_effort};
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use fact::{ExecutionMode, Problem};
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use hand::task_handled_by;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use rank::rank;

/// The model of a row whose runs recorded none.
const MODEL_UNKNOWN: &str = "model unknown";

/// The crew of a row whose attempt no job run records.
const CREW_UNKNOWN: &str = "crew unknown";

const DEFAULT_WINDOW: &str = "30d";

const DEFAULT_MIN_SUPPORT: usize = 10;

/// The selectable count windows: name and how many of the newest delivery
/// attempts in the scan the window reaches back over. They describe the
/// work, not the calendar, so a break between deliveries never empties them
/// while the scan still holds the work.
const COUNT_WINDOWS: &[(&str, usize)] = &[
    ("last50", 50),
    ("last200", 200),
    ("last500", 500),
    ("last1000", 1000),
];

/// The selectable windows: requested name, the window served and its hours.
/// `all` is served as 90 days, the scan's reach.
const WINDOWS: &[(&str, &str, i64)] = &[
    ("1h", "1h", 1),
    ("24h", "24h", 24),
    ("7d", "7d", 7 * 24),
    ("30d", "30d", 30 * 24),
    ("90d", "90d", fact::SCAN_DAYS * 24),
    ("all", "90d", fact::SCAN_DAYS * 24),
];

/// An agent role, each with its own scorecard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AgentRole {
    Implementers,
    Reviewers,
    Pilots,
}

impl AgentRole {
    pub const ALL: [AgentRole; 3] = [
        AgentRole::Implementers,
        AgentRole::Reviewers,
        AgentRole::Pilots,
    ];

    /// A role by its wire name.
    pub fn parse(name: &str) -> Result<Self, OrbitError> {
        Self::ALL
            .into_iter()
            .find(|role| role.as_str() == name.trim())
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "role must be one of implementers, reviewers or pilots; got '{name}'"
                ))
            })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AgentRole::Implementers => "implementers",
            AgentRole::Reviewers => "reviewers",
            AgentRole::Pilots => "pilots",
        }
    }
}

/// Which tasks implementer metrics count, by their derived
/// `ExecutionMode`. Unclassified tasks are in none. Cost and run time
/// count managed tasks whatever the scope: interactive work records none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionScope {
    /// Managed tasks only, the default.
    #[default]
    Managed,
    /// Managed and interactive tasks together.
    All,
    /// Interactive tasks only, as a separate series.
    Interactive,
}

impl ExecutionScope {
    const ALL: [ExecutionScope; 3] = [
        ExecutionScope::Managed,
        ExecutionScope::All,
        ExecutionScope::Interactive,
    ];

    /// The scope's wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionScope::Managed => "managed",
            ExecutionScope::All => "all",
            ExecutionScope::Interactive => "interactive",
        }
    }

    /// Whether a task executed as `mode` is counted.
    pub(crate) fn admits(self, mode: ExecutionMode) -> bool {
        match self {
            ExecutionScope::Managed => mode == ExecutionMode::Managed,
            ExecutionScope::All => mode != ExecutionMode::Unclassified,
            ExecutionScope::Interactive => mode == ExecutionMode::Interactive,
        }
    }
}

/// A validated selection: the window, its optional exclusive end, the units
/// a row needs before it is ranked and which executions implementer metrics
/// count.
#[derive(Debug, Clone, Copy)]
pub struct AgentQuery {
    requested: &'static str,
    window: &'static str,
    span: WindowSpan,
    min_support: usize,
    execution: ExecutionScope,
    /// Exclusive end. `None` leaves the upper bound open, so an omitted
    /// `until` counts the same work as a window that ends now.
    until: Option<DateTime<Utc>>,
}

/// How far back a window reaches.
#[derive(Debug, Clone, Copy)]
enum WindowSpan {
    /// This many hours ending at `until`, or now when `until` is omitted.
    Hours(i64),
    /// Back to the start of the newest this many delivery attempts.
    Attempts(usize),
}

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
            Command::Rank(query) => Outcome::Value(rank::rank(runtime, &query)?),
            Command::RankCalibrate(query) => {
                Outcome::Value(rank::agent_rank_calibrate(runtime, &query)?)
            }
            Command::VisionProbe {
                crews,
                timeout_seconds,
            } => Outcome::VisionProbe(runtime.vision_probe(&crews, timeout_seconds)?),
            Command::Scan => Outcome::Scan(Box::new(api::agent_scan(runtime)?)),
            Command::TaskTypes => Outcome::TaskTypes(runtime.task_types().to_vec()),
            Command::RankSeasons => Outcome::RankSeasons(rank::agent_rank_seasons(runtime)?),
            Command::CiFailures(query) => {
                Outcome::Value(ci_failure::ci_failure_records(runtime, &query)?)
            }
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

/// Every role scorecard's term keys and labels, in role order. The crew
/// memory score-blind lint refuses a note that quotes them, so a model's
/// memory describes its work rather than how the work is measured.
pub(crate) fn scorecard_term_vocabulary() -> Vec<&'static str> {
    AgentRole::ALL
        .into_iter()
        .flat_map(|role| role::spec(role).terms.iter())
        .flat_map(|term| [term.key, term.label])
        .collect()
}

#[cfg(test)]
mod tests;
