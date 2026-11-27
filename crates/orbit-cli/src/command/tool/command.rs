use clap::{Args, Subcommand};
use orbit_common::Env;
use orbit_core::Core;

use crate::command::{CommandOut, ExecuteCore};

use super::add::ToolAddArgs;
use super::disable::ToolDisableArgs;
use super::doctor::{self, ToolDoctorArgs};
use super::enable::ToolEnableArgs;
use super::list::ToolListArgs;
use super::remove::ToolRemoveArgs;
use super::run::ToolRunArgs;
use super::show::ToolShowArgs;

const TOOL_COMMAND_AFTER_HELP: &str = "\
Examples:
  orbit tool add ./plugins/hello_orbit.py
  orbit tool show demo.hello
  orbit tool run demo.hello --input '{}'
";

#[derive(Args)]
#[command(
    about = "Manage and run Orbit tools, including external MCP plugins",
    after_help = TOOL_COMMAND_AFTER_HELP
)]
pub struct ToolCommand {
    #[command(subcommand)]
    pub command: ToolSubcommand,
}

impl ToolCommand {
    pub(crate) fn execute_with_env(self, core: &Core, env: &Env) -> CommandOut {
        self.command.execute_with_env(core, env)
    }
}

#[derive(Subcommand)]
pub enum ToolSubcommand {
    /// List all registered tools
    List(ToolListArgs),
    /// Show detailed information about a tool
    Show(ToolShowArgs),
    /// Execute a tool
    Run(ToolRunArgs),
    /// Register an external tool or MCP plugin
    Add(ToolAddArgs),
    /// Remove an external tool
    Remove(ToolRemoveArgs),
    /// Enable a disabled tool
    Enable(ToolEnableArgs),
    /// Disable a tool
    Disable(ToolDisableArgs),
    /// Validate tool health
    Doctor(ToolDoctorArgs),
}

impl ToolSubcommand {
    fn execute_with_env(self, core: &Core, env: &Env) -> CommandOut {
        match self {
            ToolSubcommand::List(args) => args.execute(core),
            ToolSubcommand::Show(args) => args.execute(core),
            ToolSubcommand::Run(args) => args.execute_with_env(core, env),
            ToolSubcommand::Add(args) => args.execute(core),
            ToolSubcommand::Remove(args) => args.execute(core),
            ToolSubcommand::Enable(args) => args.execute(core),
            ToolSubcommand::Disable(args) => args.execute(core),
            ToolSubcommand::Doctor(args) => doctor::execute_tool_doctor(core, args.name.as_deref()),
        }
    }
}
