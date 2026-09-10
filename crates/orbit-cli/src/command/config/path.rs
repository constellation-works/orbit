use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, CommandOutput, Execute};

use super::support::global_config_path;

#[derive(Args)]
pub struct ConfigPathArgs {
    /// Print the global config.toml path instead of the effective one
    #[arg(long)]
    pub global: bool,
}

impl Execute for ConfigPathArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let path = if self.global {
            global_config_path(runtime)
        } else {
            runtime.config_path()?
        };
        println!("{}", path.to_string_lossy());
        Ok(CommandOutput::Silent)
    }
}
