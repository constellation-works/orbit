use clap::Args;
use orbit_common::governance::evolution::EvolutionVerb;
use orbit_core::Core;
use orbit_core::app::evolution::{self, Command, Input};

use crate::command::operation_args::Invocation;
use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
#[command(about = orbit_common::governance::evolution::EVOLUTION.cli_about)]
pub struct EvolutionCommand {
    #[command(subcommand)]
    pub command: Invocation<EvolutionVerb>,
}

impl ExecuteCore for EvolutionCommand {
    fn execute(self, core: &Core) -> CommandOut {
        let Invocation { spec, input } = self.command;
        let value = evolution::exec(core, Command::new(spec.verb, Input::new(input)))?.into_value();
        let block = match spec.verb {
            EvolutionVerb::List => super::list::block(&value),
            EvolutionVerb::Add => super::add::block(&value),
            EvolutionVerb::Show => super::show::block(&value),
            EvolutionVerb::Decline => super::decline::block(&value),
        };
        Ok(Payload::blocks(value, vec![block]).into())
    }
}
