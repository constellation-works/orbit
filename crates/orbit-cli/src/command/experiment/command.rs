use clap::Args;
use orbit_common::governance::experiment::ExperimentVerb;
use orbit_core::OrbitRuntime;
use serde_json::Value;

use super::{
    abandon, assign, clone, conclude, create, delete, list, pause, readout, resume, reweight, show,
    start, update,
};
use crate::command::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
#[command(about = orbit_common::governance::experiment::EXPERIMENT.cli_about)]
pub struct ExperimentCommand {
    #[command(subcommand)]
    pub command: Invocation<ExperimentVerb>,
}

impl Execute for ExperimentCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, input } = self.command;
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        let blocks = match spec.verb {
            ExperimentVerb::Assign => assign::blocks(&value),
            ExperimentVerb::Delete => delete::blocks(&value),
            ExperimentVerb::List => list::blocks(&value),
            ExperimentVerb::Readout => readout::blocks(&value),
            ExperimentVerb::Create => create::blocks(&value),
            ExperimentVerb::Update => update::blocks(&value),
            ExperimentVerb::Clone => clone::blocks(&value),
            ExperimentVerb::Start => start::blocks(&value),
            ExperimentVerb::Pause => pause::blocks(&value),
            ExperimentVerb::Resume => resume::blocks(&value),
            ExperimentVerb::Reweight => reweight::blocks(&value),
            ExperimentVerb::Conclude => conclude::blocks(&value),
            ExperimentVerb::Abandon => abandon::blocks(&value),
            ExperimentVerb::Show => show::blocks(&value),
        };
        Ok(Payload::blocks(value, blocks).into())
    }
}
