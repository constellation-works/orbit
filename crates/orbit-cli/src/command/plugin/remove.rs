use clap::Args;
use orbit_core::Core;
use orbit_core::adapter::command::PluginRemoveOptions;
use orbit_core::app::plugin::{Command, Outcome, exec};
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
pub struct PluginRemoveArgs {
    /// Plugin namespace
    pub name: String,
    /// Confirm permanent removal of the plugin install
    #[arg(long, visible_alias = "yes")]
    pub confirm: bool,
    /// Remove only this host's record of the plugin, leaving every installed
    /// file in place. Clears a record whose install path this host refuses,
    /// without deleting the path it names
    #[arg(long)]
    pub record_only: bool,
    /// Also delete this plugin's Orbit-owned state directory
    #[arg(long, conflicts_with = "record_only")]
    pub purge_state: bool,
}

impl ExecuteCore for PluginRemoveArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Removed(state_dir) = exec(
            core,
            Command::Remove {
                name: self.name.clone(),
                options: PluginRemoveOptions {
                    record_only: self.record_only,
                    purge_state: self.purge_state,
                },
                confirmed: self.confirm,
            },
        )?
        else {
            unreachable!("plugin command returned an unrelated outcome");
        };
        let state_message = if self.purge_state {
            format!("Plugin state at {} was removed.", state_dir.display())
        } else {
            format!("Plugin state retained at {}.", state_dir.display())
        };
        let text = if self.record_only {
            format!(
                "Removed this host's record of plugin '{}'. The installed files were left in \
                 place. {state_message}",
                self.name,
            )
        } else {
            format!(
                "Removed plugin '{}'. {state_message} Data the plugin wrote outside that state \
                 directory is retained.",
                self.name,
            )
        };
        Ok(Payload::detail(
            json!({
                "name": self.name,
                "removed": true,
                "record_only": self.record_only,
                "install_removed": !self.record_only,
                "plugin_data_retained": !self.purge_state,
                "plugin_state_removed": self.purge_state,
                "plugin_state_path": state_dir,
            }),
            text,
        )
        .into())
    }
}
