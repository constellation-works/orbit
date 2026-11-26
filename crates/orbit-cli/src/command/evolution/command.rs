use clap::Args;
use orbit_common::governance::evolution::EvolutionVerb;
use orbit_core::OrbitRuntime;
use serde_json::Value;

use crate::command::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
#[command(about = orbit_common::governance::evolution::EVOLUTION.cli_about)]
pub struct EvolutionCommand {
    #[command(subcommand)]
    pub command: Invocation<EvolutionVerb>,
}
impl Execute for EvolutionCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, input } = self.command;
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        let block = match spec.verb {
            EvolutionVerb::List => super::list::block(&value),
            EvolutionVerb::Add => super::add::block(&value),
            EvolutionVerb::Show => super::show::block(&value),
            EvolutionVerb::Decline => super::decline::block(&value),
        };
        Ok(Payload::blocks(value, vec![block]).into())
    }
}
