use clap::Args;
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct PluginRemoveArgs {
    /// Plugin namespace
    pub name: String,
    /// Confirm permanent removal of the plugin install
    #[arg(long)]
    pub yes: bool,
}

impl Execute for PluginRemoveArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if !self.yes {
            return Err(OrbitError::InvalidInput(format!(
                "removing plugin '{}' is irreversible; pass --yes to proceed",
                self.name
            )));
        }
        runtime.remove_plugin(&self.name)?;
        let text = format!(
            "Removed plugin '{}'. Data the plugin wrote elsewhere is retained.",
            self.name
        );
        Ok(Payload::detail(
            json!({
                "name": self.name,
                "removed": true,
                "plugin_data_retained": true,
            }),
            text,
        )
        .into())
    }
}
