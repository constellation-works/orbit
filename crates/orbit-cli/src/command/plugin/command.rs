use clap::{Args, Subcommand};
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute};

use super::add::PluginAddArgs;
use super::disable::PluginDisableArgs;
use super::doctor::execute_doctor;
use super::enable::PluginEnableArgs;
use super::list::PluginListArgs;
use super::migrate::PluginMigrateArgs;
use super::remove::PluginRemoveArgs;
use super::scaffold::PluginScaffoldArgs;
use super::show::PluginShowArgs;
use super::sync::PluginSyncArgs;
use super::test::PluginTestArgs;
use super::validate::PluginValidateArgs;

const PLUGIN_COMMAND_AFTER_HELP: &str = "\
Examples:
  orbit plugin scaffold demo
  orbit plugin validate ./demo
  orbit plugin test ./demo
  orbit plugin add ./demo --enable
  orbit plugin list

Plugins install once per machine under the Orbit global root; a repository
commits only the `.orbit/plugins.yaml` pin file, never a plugin tree.
";

#[derive(Args)]
#[command(
    about = "Install and manage Orbit plugins (`plugin.yaml` v2)",
    after_help = PLUGIN_COMMAND_AFTER_HELP
)]
pub struct PluginCommand {
    #[command(subcommand)]
    pub command: PluginSubcommand,
}

impl Execute for PluginCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        self.command.execute(runtime)
    }
}

#[derive(Subcommand)]
pub enum PluginSubcommand {
    /// Install a plugin for this machine
    Add(PluginAddArgs),
    /// Put an installed plugin's tools on the tool surface
    Enable(PluginEnableArgs),
    /// Take a plugin's tools off the tool surface
    Disable(PluginDisableArgs),
    /// Uninstall a plugin from this machine
    Remove(PluginRemoveArgs),
    /// List installed and pinned plugins
    List(PluginListArgs),
    /// Show one plugin, its tools, and its requested versus granted permissions
    Show(PluginShowArgs),
    /// Report what each plugin needs before it can serve its tools
    Doctor,
    /// Check a plugin directory without installing it
    Validate(PluginValidateArgs),
    /// Run a plugin's conformance goldens against this Orbit
    Test(PluginTestArgs),
    /// Generate a starter plugin: backend, tool, panel, skill and goldens
    Scaffold(PluginScaffoldArgs),
    /// Install what this workspace pins but the machine is missing
    Sync(PluginSyncArgs),
    /// Write a v2 manifest from v1 `*.orbit-tool.yaml` sidecars
    Migrate(PluginMigrateArgs),
}

impl Execute for PluginSubcommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self {
            PluginSubcommand::Add(args) => args.execute(runtime),
            PluginSubcommand::Enable(args) => args.execute(runtime),
            PluginSubcommand::Disable(args) => args.execute(runtime),
            PluginSubcommand::Remove(args) => args.execute(runtime),
            PluginSubcommand::List(args) => args.execute(runtime),
            PluginSubcommand::Show(args) => args.execute(runtime),
            PluginSubcommand::Doctor => execute_doctor(runtime),
            PluginSubcommand::Validate(args) => args.execute(runtime),
            PluginSubcommand::Test(args) => args.execute(runtime),
            PluginSubcommand::Scaffold(args) => args.execute(runtime),
            PluginSubcommand::Sync(args) => args.execute(runtime),
            PluginSubcommand::Migrate(args) => args.execute(runtime),
        }
    }
}
