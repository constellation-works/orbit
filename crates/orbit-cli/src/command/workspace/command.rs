use clap::{Args, Subcommand};
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute};

use super::init::WorkspaceInitArgs;
use super::list::WorkspaceListArgs;
use super::publication::WorkspacePublicationCommand;
use super::remove::WorkspaceRemoveArgs;
use super::role::WorkspaceRoleArgs;
use super::show::WorkspaceShowArgs;
use super::source_remote::WorkspaceSourceRemoteCommand;
use super::sync::WorkspaceSyncArgs;
use super::teardown::WorkspaceTeardownArgs;

#[derive(Args)]
#[command(about = "Initialize and manage workspaces")]
pub struct WorkspaceCommand {
    #[command(subcommand)]
    pub command: WorkspaceSubcommand,
}

#[derive(Subcommand)]
pub enum WorkspaceSubcommand {
    /// Initialize a new workspace in the current directory
    Init(WorkspaceInitArgs),
    /// Converge managed artifacts on the defaults shipped by this Orbit binary
    Sync(WorkspaceSyncArgs),
    /// List all registered workspaces
    List(WorkspaceListArgs),
    /// Show the current workspace
    Show(WorkspaceShowArgs),
    /// Inspect or explicitly rebind the source Git remote
    SourceRemote(WorkspaceSourceRemoteCommand),
    /// Validate or reassert this checkout's declared local role
    Role(WorkspaceRoleArgs),
    /// Manage the owner-local task-publication repository binding
    Publication(WorkspacePublicationCommand),
    /// Remove a workspace from the registry (does not delete .orbit)
    Remove(WorkspaceRemoveArgs),
    /// Remove all Orbit artifacts from a named workspace
    Teardown(WorkspaceTeardownArgs),
}

impl Execute for WorkspaceCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self.command {
            WorkspaceSubcommand::Init(_) => {
                // Init is handled without runtime in main.rs
                unreachable!("workspace init should be handled before runtime initialization")
            }
            WorkspaceSubcommand::Sync(_) => {
                unreachable!("workspace sync should be handled before runtime initialization")
            }
            WorkspaceSubcommand::List(args) => args.execute(runtime),
            WorkspaceSubcommand::Show(args) => args.execute(runtime),
            WorkspaceSubcommand::SourceRemote(command) => command.execute(runtime),
            WorkspaceSubcommand::Role(args) => args.execute(runtime),
            WorkspaceSubcommand::Publication(command) => command.execute(runtime),
            WorkspaceSubcommand::Remove(args) => args.execute(runtime),
            WorkspaceSubcommand::Teardown(args) => args.execute(runtime),
        }
    }
}
