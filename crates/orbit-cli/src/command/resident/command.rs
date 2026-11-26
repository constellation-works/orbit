use clap::Args;
use orbit_common::governance::resident::ResidentVerb;
use orbit_core::Core;
use orbit_core::app::resident::{self, Command, Input};

use super::{checkpoint, control, digest, grant, list, metrics, relay, show};
use crate::command::operation_args::Invocation;
use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
#[command(about = orbit_common::governance::resident::RESIDENT.cli_about)]
pub struct ResidentCommand {
    #[command(subcommand)]
    pub command: Invocation<ResidentVerb>,
}

impl ExecuteCore for ResidentCommand {
    fn execute(self, core: &Core) -> CommandOut {
        let Invocation { spec, input } = self.command;
        let value =
            resident::exec(core, Command::Operation(spec.verb, Input::new(input)))?.into_value();
        let block = match spec.verb {
            ResidentVerb::Checkpoint => checkpoint::block(&value),
            ResidentVerb::Grant => grant::block(&value),
            ResidentVerb::Relay => relay::block(&value),
            ResidentVerb::Control => control::block(&value),
            ResidentVerb::Digest => digest::block(&value),
            ResidentVerb::Metrics => metrics::block(&value),
            ResidentVerb::Show => show::block(&value),
            ResidentVerb::List => list::block(&value),
        };
        Ok(Payload::blocks(value, vec![block]).into())
    }
}
