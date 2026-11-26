use clap::Args;
use orbit_common::governance::resident::ResidentVerb;
use orbit_core::OrbitRuntime;
use serde_json::Value;

use super::{checkpoint, control, digest, grant, list, metrics, relay, show};
use crate::command::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
#[command(about = orbit_common::governance::resident::RESIDENT.cli_about)]
pub struct ResidentCommand {
    #[command(subcommand)]
    pub command: Invocation<ResidentVerb>,
}

impl Execute for ResidentCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, input } = self.command;
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
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
