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

mod assignment;

mod capture;

mod definition;

mod distributed_port;

mod job_port;

mod lifecycle;

mod memory_arm;

mod operation;

mod overlay;

mod enrollment;

mod promotion;

mod readout;

mod record;

mod run_policy;

mod skill;

mod tag_instruction_port;

mod task_port;

mod view;

use chrono::{DateTime, Datelike, Days, Utc};
use orbit_common::OrbitError;
use orbit_common::governance::experiment::ExperimentVerb;
use serde_json::Value;

use super::exec::{self, App, Operation};
use super::tool_call;
pub use super::tool_call::Input;
use crate::Core;

pub(crate) use capture::validate_claimed_experiment_capture;
pub use definition::{EXPERIMENT_COMPLEXITIES, experiment_metric_names, step_local_metric_names};
pub(crate) use distributed_port::DistributedPort;
pub use enrollment::ExperimentProgress;
pub(crate) use enrollment::experiment_progress;
pub(crate) use job_port::JobPort;
pub use lifecycle::{ExperimentActor, ExperimentChange, ExperimentDefinitionInput};
use lifecycle::{clone_experiment, delete_experiment, experiment_views};
pub(crate) use memory_arm::running_memory_experiments;
pub(crate) use overlay::experiment_overlays_for_activity;
pub use readout::ExperimentReadout;
pub(crate) use tag_instruction_port::TagInstructionPort;
pub(crate) use task_port::TaskPort;
pub use view::{ExperimentArmView, ExperimentEventView, ExperimentView};

#[cfg(not(feature = "test-support"))]
use assignment::force_experiment_assignment;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use assignment::{force_experiment_assignment, task_experiments};
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use lifecycle::{change_experiment, create_experiment, experiment_view, update_experiment};
#[cfg(not(feature = "test-support"))]
pub(crate) use lifecycle::{
    change_experiment, create_experiment, experiment_view, update_experiment,
};
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use memory_arm::crew_memory_experiment_running;
#[cfg(not(feature = "test-support"))]
pub(crate) use memory_arm::crew_memory_experiment_running;
pub(crate) use memory_arm::task_in_memory_arm;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use readout::experiment_readout;
#[cfg(not(feature = "test-support"))]
pub(crate) use readout::experiment_readout;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use record::{assign_experiments, write_experiment};

/// Experiments that may run at once in one workspace.
///
/// The former `experiments.max_running` default, fixed when the key was removed [ORB-22808].
pub const EXPERIMENTS_MAX_RUNNING: u32 = 3;

/// Share of a treatment arm's settled tasks that must have received its
/// treatment; below it, a readout gives no verdict.
///
/// The former `experiments.min_delivered_rate` default, fixed when the key was removed [ORB-22808].
pub(crate) const EXPERIMENTS_MIN_DELIVERED_RATE: f64 = 0.9;

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

/// Use the four-digit RFC 3339 calendar shared by the API and the dashboard.
fn experiment_horizon_at(start: DateTime<Utc>, days: u32) -> Option<DateTime<Utc>> {
    start
        .checked_add_days(Days::new(u64::from(days)))
        .filter(|end| days > 0 && (1..=9999).contains(&end.year()))
}
