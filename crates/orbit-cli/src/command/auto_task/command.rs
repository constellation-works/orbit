use clap::{Args, Subcommand};
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute};

use super::add::AutoTaskAddArgs;
use super::list::AutoTaskListArgs;
use super::mint::AutoTaskMintArgs;
use super::recover::AutoTaskRecoverArgs;
use super::reset::AutoTaskResetArgs;
use super::show::AutoTaskShowArgs;
use super::toggle::AutoTaskToggleArgs;
use super::update::AutoTaskUpdateArgs;

#[derive(Args)]
#[command(about = "Define and manage recurring auto-task templates")]
pub struct AutoTaskCommand {
    #[command(subcommand)]
    pub command: AutoTaskSubcommand,
}

impl Execute for AutoTaskCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        self.command.execute(runtime)
    }
}

#[derive(Subcommand)]
pub enum AutoTaskSubcommand {
    /// Create a new auto-task definition
    Add(AutoTaskAddArgs),
    /// List every auto-task definition in this workspace
    List(AutoTaskListArgs),
    /// Show a single definition by name
    Show(AutoTaskShowArgs),
    /// Update an existing definition (present fields only)
    Update(AutoTaskUpdateArgs),
    /// Enable or disable a definition (the kill-switch; not a delete)
    Toggle(AutoTaskToggleArgs),
    /// Mint a task from a definition now (ignores schedule, dedupe, and
    /// `enabled`; leaves the scheduler cursor untouched)
    Mint(AutoTaskMintArgs),
    /// Preview or apply the audited recovery for a delivery consumer stalled
    /// by a settings change (retains all coverage debt)
    Recover(AutoTaskRecoverArgs),
    /// Preview or apply the audited reset of a delivery consumer: forget its
    /// coverage debt and re-baseline at the branch head
    Reset(AutoTaskResetArgs),
}

impl Execute for AutoTaskSubcommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self {
            AutoTaskSubcommand::Add(args) => args.execute(runtime),
            AutoTaskSubcommand::List(args) => args.execute(runtime),
            AutoTaskSubcommand::Show(args) => args.execute(runtime),
            AutoTaskSubcommand::Update(args) => args.execute(runtime),
            AutoTaskSubcommand::Toggle(args) => args.execute(runtime),
            AutoTaskSubcommand::Mint(args) => args.execute(runtime),
            AutoTaskSubcommand::Recover(args) => args.execute(runtime),
            AutoTaskSubcommand::Reset(args) => args.execute(runtime),
        }
    }
}
