use clap::{Args, Subcommand};
use orbit_common::governance::agents::AgentsVerb;
use orbit_core::Core;
use orbit_core::app::agent::{self, Command, Input};

use super::rank::{self, AgentsRankAction, AgentsRankArgs, RANK_LONG_ABOUT};
use super::vision_probe::{self, AgentsVisionProbeArgs, VISION_PROBE_LONG_ABOUT};
use super::{ci_failures, scorecard, show};
use crate::command::operation_args::Invocation;
use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
#[command(about = orbit_common::governance::agents::AGENTS.cli_about)]
pub struct AgentsCommand {
    #[command(subcommand)]
    pub command: AgentsSubcommand,
}

impl ExecuteCore for AgentsCommand {
    fn execute(self, core: &Core) -> CommandOut {
        match self.command {
            AgentsSubcommand::Operation(invocation) => {
                let Invocation { spec, input } = invocation;
                let value = agent::exec(core, Command::Operation(spec.verb, Input::new(input)))?
                    .into_value()?;
                let blocks = match spec.verb {
                    AgentsVerb::Scorecard => scorecard::blocks(&value),
                    AgentsVerb::Show => show::blocks(&value),
                    AgentsVerb::CiFailures => ci_failures::blocks(&value),
                };
                Ok(Payload::blocks(value, blocks).into())
            }
            AgentsSubcommand::Rank(args) => {
                let (value, blocks) = match &args.action {
                    None => {
                        let command = Command::Rank(args.selection.query(args.live)?);
                        let value = agent::exec(core, command)?.into_value()?;
                        let blocks = rank::rank_blocks(&value);
                        (value, blocks)
                    }
                    Some(AgentsRankAction::Calibrate(selection)) => {
                        let command = Command::RankCalibrate(selection.query(false)?);
                        let value = agent::exec(core, command)?.into_value()?;
                        let blocks = rank::calibrate_blocks(&value);
                        (value, blocks)
                    }
                };
                Ok(Payload::blocks(value, blocks).into())
            }
            AgentsSubcommand::VisionProbe(args) => {
                let command = Command::VisionProbe {
                    crews: args.crews,
                    timeout_seconds: args.timeout_seconds,
                };
                let value = agent::exec(core, command)?.into_value()?;
                let blocks = vision_probe::vision_probe_blocks(&value);
                Ok(Payload::blocks(value, blocks).into())
            }
        }
    }
}

#[derive(Subcommand)]
pub enum AgentsSubcommand {
    // Scorecard, show and ci-failures, derived from the agents operation registry.
    #[command(flatten)]
    Operation(Invocation<AgentsVerb>),
    /// Rank implementer models by quality and reliability, in tie bands
    #[command(long_about = RANK_LONG_ABOUT)]
    Rank(AgentsRankArgs),
    /// Check that each crew's model sees images: run the canary per crew
    #[command(long_about = VISION_PROBE_LONG_ABOUT)]
    VisionProbe(AgentsVisionProbeArgs),
}
