use clap::{Args, Subcommand};
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute};

use super::rename::HostRenameArgs;
use super::show::HostShowArgs;

#[derive(Args)]
#[command(about = "Manage this machine's local host identity")]
pub struct HostCommand {
    #[command(subcommand)]
    pub command: HostSubcommand,
}

impl Execute for HostCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        self.command.execute(runtime)
    }
}

#[derive(Subcommand)]
pub enum HostSubcommand {
    /// Show this machine's local host identity without changing it
    Show(HostShowArgs),
    /// Rename this machine in host.toml and its local workspace owner records
    Rename(HostRenameArgs),
}

impl Execute for HostSubcommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self {
            HostSubcommand::Show(args) => args.execute(runtime),
            HostSubcommand::Rename(args) => args.execute(runtime),
        }
    }
}
