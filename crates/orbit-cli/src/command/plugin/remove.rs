use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, CommandOutput, Execute};

#[derive(Args)]
pub struct PluginRemoveArgs {
    /// Plugin namespace
    pub name: String,
}

impl Execute for PluginRemoveArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        runtime.remove_plugin(&self.name)?;
        println!(
            "Removed plugin '{}'. Data the plugin wrote elsewhere is retained.",
            self.name
        );
        Ok(CommandOutput::Silent)
    }
}
