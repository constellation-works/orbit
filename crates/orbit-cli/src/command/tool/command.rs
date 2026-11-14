use clap::{Args, Subcommand};
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute};

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

impl Execute for ToolCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        self.command.execute(runtime)
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

impl Execute for ToolSubcommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self {
            ToolSubcommand::List(args) => args.execute(runtime),
            ToolSubcommand::Show(args) => args.execute(runtime),
            ToolSubcommand::Run(args) => args.execute(runtime),
            ToolSubcommand::Add(args) => args.execute(runtime),
            ToolSubcommand::Remove(args) => args.execute(runtime),
            ToolSubcommand::Enable(args) => args.execute(runtime),
            ToolSubcommand::Disable(args) => args.execute(runtime),
            ToolSubcommand::Doctor(args) => {
                doctor::execute_tool_doctor(runtime, args.name.as_deref())
            }
        }
    }
}
