use clap::{Args, Subcommand};
use orbit_common::Env;
use orbit_core::Core;

use crate::command::browser_kit::BrowserKitCommand;
use crate::command::{CommandOut, ExecuteCore};

use super::get::ConfigGetArgs;
use super::keys::ConfigKeysArgs;
use super::path::ConfigPathArgs;
use super::set::ConfigSetArgs;
use super::show::ConfigShowArgs;

#[derive(Args)]
#[command(about = "Show or update Orbit configuration")]
pub struct ConfigCommand {
    #[command(subcommand)]
    pub command: ConfigSubcommand,
}

impl ConfigCommand {
    pub(crate) fn execute_with_env(self, core: &Core, env: &Env) -> CommandOut {
        self.command.execute_with_env(core, env)
    }
}

#[derive(Subcommand)]
pub enum ConfigSubcommand {
    /// Display configuration values, with source provenance
    Show(ConfigShowArgs),
    /// Get a single configuration value
    Get(ConfigGetArgs),
    /// Set a configuration value
    Set(ConfigSetArgs),
    /// List all settable configuration keys
    Keys(ConfigKeysArgs),
    /// Print the resolved config.toml path
    Path(ConfigPathArgs),
    /// Install, verify and remove the headless browser kit workers check UIs with
    #[command(name = "browser-kit")]
    BrowserKit(BrowserKitCommand),
}

impl ConfigSubcommand {
    fn execute_with_env(self, core: &Core, env: &Env) -> CommandOut {
        match self {
            ConfigSubcommand::Show(args) => args.execute(core),
            ConfigSubcommand::Get(args) => args.execute(core),
            ConfigSubcommand::Set(args) => args.execute_with_env(core, env),
            ConfigSubcommand::Keys(args) => args.execute(core),
            ConfigSubcommand::Path(args) => args.execute(core),
            ConfigSubcommand::BrowserKit(command) => command.execute_without_runtime(env),
        }
    }
}
