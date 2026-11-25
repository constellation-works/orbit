use clap::{Args, Subcommand};
use orbit_common::Env;
use orbit_core::OrbitRuntime;

use crate::command::CommandOut;

use super::tail;

#[derive(Args)]
#[command(about = "Inspect the unified Orbit log feed")]
pub struct LogCommand {
    #[command(subcommand)]
    pub command: LogSubcommand,
}

impl LogCommand {
    pub(crate) fn execute_with_env(self, runtime: &OrbitRuntime, env: &Env) -> CommandOut {
        self.command.execute_with_env(runtime, env)
    }
}

#[derive(Subcommand)]
pub enum LogSubcommand {
    /// Tail the unified Orbit log feed (`~/.orbit/state/logs/orbit.jsonl`)
    Tail(tail::TailArgs),
}

impl LogSubcommand {
    fn execute_with_env(self, runtime: &OrbitRuntime, env: &Env) -> CommandOut {
        match self {
            LogSubcommand::Tail(args) => args.execute_with_env(runtime, env),
        }
    }
}
