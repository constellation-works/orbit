use std::path::Path;

use clap::{Args, Subcommand};

use super::add::{self, HostAddArgs};
use super::list::{self, HostListArgs};
use super::offline::{self, HostOfflineArgs};
use super::online;
use super::remove::{self, HostRemoveArgs};
use super::rename::{self, HostRenameArgs};
use super::show;
use super::target::HostShowArgs;
use crate::command::CommandOut;
use crate::entry::resolved::Resolved;

#[derive(Args)]
#[command(
    about = "Register and inspect the remote Orbit hosts this machine reaches over SSH",
    long_about = "Register and inspect the remote Orbit hosts this machine reaches over SSH\n\n\
        Federated MCP, pull drains and replica worktree GC route to the hosts registered here. \
        Each entry stores the host's name, SSH target, machine_id and task prefix, read from the \
        host itself when it is added. Reachability, version, pull protocol and workspaces are \
        read live from each host whenever a command asks. Use offline and online to acknowledge \
        planned downtime while keeping the host registered."
)]
pub struct HostCommand {
    #[command(subcommand)]
    pub command: HostSubcommand,
}

#[derive(Subcommand)]
pub enum HostSubcommand {
    /// Register a remote host, reading its identity from the host over SSH
    Add(HostAddArgs),
    /// List this host and every registered host with live reachability, version and protocol
    List(HostListArgs),
    /// Show one host and what on this machine routes to it
    Show(HostShowArgs),
    /// Rename a registered host
    Rename(HostRenameArgs),
    /// Mark a registered host as intentionally offline
    Offline(HostOfflineArgs),
    /// Clear a registered host's offline acknowledgement
    Online(HostShowArgs),
    /// Remove a registered host
    Remove(HostRemoveArgs),
}

impl HostCommand {
    pub fn execute_without_runtime(self, resolved: &Resolved) -> CommandOut {
        let global_root = resolved.generation_root()?;
        self.command.execute_without_runtime(global_root)
    }
}

impl HostSubcommand {
    fn execute_without_runtime(self, global_root: &Path) -> CommandOut {
        match self {
            Self::Add(args) => add::execute(args, global_root),
            Self::List(args) => list::execute(args, global_root),
            Self::Show(args) => show::execute(args, global_root),
            Self::Rename(args) => rename::execute(args, global_root),
            Self::Offline(args) => offline::execute(args, global_root),
            Self::Online(args) => online::execute(args, global_root),
            Self::Remove(args) => remove::execute(args, global_root),
        }
    }
}
