use clap::{Args, Subcommand};
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

impl Execute for ConfigCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        self.command.execute(runtime)
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

impl Execute for ConfigSubcommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self {
            ConfigSubcommand::Show(args) => args.execute(runtime),
            ConfigSubcommand::Get(args) => args.execute(runtime),
            ConfigSubcommand::Set(args) => args.execute(runtime),
            ConfigSubcommand::Keys(args) => args.execute(runtime),
            ConfigSubcommand::Path(args) => args.execute(runtime),
            ConfigSubcommand::BrowserKit(command) => command.execute_without_runtime(),
        }
    }
}
