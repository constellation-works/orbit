use clap::{Args, Subcommand};
use orbit_core::Core;

use crate::command::{CommandOut, ExecuteCore};

use super::tail;

#[derive(Args)]
#[command(about = "Inspect the unified Orbit log feed")]
pub struct LogCommand {
    #[command(subcommand)]
    pub command: LogSubcommand,
}

impl ExecuteCore for LogCommand {
    fn execute(self, core: &Core) -> CommandOut {
        self.command.execute(core)
    }
}

#[derive(Subcommand)]
pub enum LogSubcommand {
    /// Tail the unified Orbit log feed (`~/.orbit/state/logs/orbit.jsonl`)
    Tail(tail::TailArgs),
}

impl ExecuteCore for LogSubcommand {
    fn execute(self, core: &Core) -> CommandOut {
        match self {
            LogSubcommand::Tail(args) => args.execute(core),
        }
    }
}
