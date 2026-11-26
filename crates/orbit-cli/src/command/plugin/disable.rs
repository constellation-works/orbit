use clap::Args;
use orbit_core::Core;
use orbit_core::app::plugin::{Command, Outcome, exec};

use crate::command::{CommandOut, ExecuteCore, Payload};

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

impl ExecuteCore for PluginDisableArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Disabled(summary) = exec(
            core,
            Command::Disable {
                name: self.name,
                workspace: self.scope == PluginScope::Workspace,
            },
        )?
        else {
            unreachable!("plugin command returned an unrelated outcome");
        };
        let text = match self.scope {
            PluginScope::Host => format!(
                "Disabled plugin '{}'. Its tools leave the tool surface on the next Orbit \
                 command.",
                summary.name
            ),
            PluginScope::Workspace => format!(
                "Disabled plugin '{}' in this workspace. Its tools leave this workspace's \
                 tool surface on the next Orbit command; other workspaces are unchanged.",
                summary.name
            ),
        };
        Ok(Payload::detail(plugin_record(&summary), text).into())
    }
}
