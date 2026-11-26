use clap::{Args, Subcommand};
use orbit_common::Env;
use orbit_core::Core;

use crate::command::{CommandOut, ExecuteCore};

use super::add::PluginAddArgs;
use super::disable::PluginDisableArgs;
use super::doctor::execute_doctor;
use super::enable::PluginEnableArgs;
use super::list::PluginListArgs;
use super::migrate::PluginMigrateArgs;
use super::remove::PluginRemoveArgs;
use super::scaffold::PluginScaffoldArgs;
use super::secret::PluginSecretCommand;
use super::show::PluginShowArgs;
use super::sync::PluginSyncArgs;
use super::test::PluginTestArgs;
use super::upgrade::PluginUpgradeArgs;
use super::validate::PluginValidateArgs;

const PLUGIN_COMMAND_AFTER_HELP: &str = "\
Examples:
  orbit plugin scaffold demo
  orbit plugin validate ./demo --render
  orbit plugin test ./demo --case status_reports_ready
  orbit plugin test ./demo --update-goldens
  orbit plugin add ./demo --enable
  orbit plugin secret set demo api_token < token.txt
  orbit plugin list

A plugin source keeps its plugin in a `.orbit-plugin/` directory holding
`plugin.yaml`; that directory is the plugin root and the only tree installed.
Plugins install once per machine under the Orbit global root. A checkout only
pins them in `.orbit/plugins.yaml`, per-checkout state that git ignores with the
rest of `.orbit/`; a plugin tree is never vendored into a checkout.
Golden input and output strings may use {{workspace}} and {{plugin_root}};
expect.error.code matches a backend's structured error code.
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

impl PluginCommand {
    pub(crate) fn execute_with_env(self, core: &Core, env: &Env) -> CommandOut {
        self.command.execute_with_env(core, env)
    }
}

#[derive(Subcommand)]
pub enum PluginSubcommand {
    /// Install a plugin for this machine
    Add(PluginAddArgs),
    /// Upgrade an installed plugin and review requested-permission changes
    Upgrade(PluginUpgradeArgs),
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
    /// Set, list or remove the secrets a plugin declares in `spec.secrets`
    Secret(PluginSecretCommand),
    /// Check a plugin source without installing it
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

impl PluginSubcommand {
    fn execute_with_env(self, core: &Core, env: &Env) -> CommandOut {
        match self {
            PluginSubcommand::Add(args) => args.execute(core),
            PluginSubcommand::Upgrade(args) => args.execute(core),
            PluginSubcommand::Enable(args) => args.execute(core),
            PluginSubcommand::Disable(args) => args.execute(core),
            PluginSubcommand::Remove(args) => args.execute(core),
            PluginSubcommand::List(args) => args.execute(core),
            PluginSubcommand::Show(args) => args.execute(core),
            PluginSubcommand::Doctor => execute_doctor(core),
            PluginSubcommand::Secret(command) => command.execute(core),
            PluginSubcommand::Validate(args) => args.execute(core),
            PluginSubcommand::Test(args) => args.execute_with_env(core, env),
            PluginSubcommand::Scaffold(args) => args.execute(core),
            PluginSubcommand::Sync(args) => args.execute(core),
            PluginSubcommand::Migrate(args) => args.execute(core),
        }
    }
}
