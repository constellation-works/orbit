use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute, Payload};

use super::support::{PluginScope, plugin_record};

#[derive(Args)]
pub struct PluginDisableArgs {
    /// Plugin namespace
    pub name: String,
    /// Which enable state to write. `host` (the default) disables the plugin
    /// on this machine for every workspace and unlinks its skills.
    /// `workspace` switches it off only in the selected workspace
    /// (`--workspace` or the current directory); the host row, other
    /// workspaces and the host-level skill links are untouched.
    #[arg(long, value_enum, default_value_t = PluginScope::Host)]
    pub scope: PluginScope,
}

impl Execute for PluginDisableArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let (summary, text) = match self.scope {
            PluginScope::Host => {
                let summary = runtime.disable_plugin(&self.name)?;
                let text = format!(
                    "Disabled plugin '{}'. Its tools leave the tool surface on the next Orbit \
                     command.",
                    summary.name
                );
                (summary, text)
            }
            PluginScope::Workspace => {
                let summary = runtime.disable_plugin_in_workspace(&self.name)?;
                let text = format!(
                    "Disabled plugin '{}' in this workspace. Its tools leave this workspace's \
                     tool surface on the next Orbit command; other workspaces are unchanged.",
                    summary.name
                );
                (summary, text)
            }
        };
        Ok(Payload::detail(plugin_record(&summary), text).into())
    }
}
