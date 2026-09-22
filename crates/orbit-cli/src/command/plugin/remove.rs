use clap::Args;
use orbit_core::adapter::command::PluginRemoveOptions;
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
    /// Remove only this host's record of the plugin, leaving every installed
    /// file in place. Clears a record whose install path this host refuses,
    /// without deleting the path it names
    #[arg(long)]
    pub record_only: bool,
}

impl Execute for PluginRemoveArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if !self.yes {
            return Err(OrbitError::InvalidInput(format!(
                "removing plugin '{}' is irreversible; pass --yes to proceed",
                self.name
            )));
        }
        runtime.remove_plugin(
            &self.name,
            &PluginRemoveOptions {
                record_only: self.record_only,
            },
        )?;
        let text = if self.record_only {
            format!(
                "Removed this host's record of plugin '{}'. The installed files were left in \
                 place.",
                self.name
            )
        } else {
            format!(
                "Removed plugin '{}'. Data the plugin wrote elsewhere is retained.",
                self.name
            )
        };
        Ok(Payload::detail(
            json!({
                "name": self.name,
                "removed": true,
                "record_only": self.record_only,
                "install_removed": !self.record_only,
                "plugin_data_retained": true,
            }),
            text,
        )
        .into())
    }
}
