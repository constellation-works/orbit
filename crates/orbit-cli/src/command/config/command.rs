use clap::{Args, Subcommand};
use orbit_common::Env;
use orbit_core::OrbitRuntime;

use crate::command::browser_kit::BrowserKitCommand;
use crate::command::{CommandOut, Execute};

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
    pub(crate) fn execute_with_env(self, runtime: &OrbitRuntime, env: &Env) -> CommandOut {
        self.command.execute_with_env(runtime, env)
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
    fn execute_with_env(self, runtime: &OrbitRuntime, env: &Env) -> CommandOut {
        match self {
            ConfigSubcommand::Show(args) => args.execute(runtime),
            ConfigSubcommand::Get(args) => args.execute(runtime),
            ConfigSubcommand::Set(args) => args.execute_with_env(runtime, env),
            ConfigSubcommand::Keys(args) => args.execute(runtime),
            ConfigSubcommand::Path(args) => args.execute(runtime),
            ConfigSubcommand::BrowserKit(command) => command.execute_without_runtime(env),
        }
    }
}
