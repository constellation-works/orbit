use clap::Args;
use orbit_core::Core;
use orbit_core::app::tool::{self, Command};
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
pub struct ToolEnableArgs {
    /// Tool name to enable
    pub name: String,
}

impl ExecuteCore for ToolEnableArgs {
    fn execute(self, core: &Core) -> CommandOut {
        tool::exec(core, Command::Enable(self.name.clone()))?;
        Ok(Payload::detail(
            json!({ "tool": self.name, "enabled": true }),
            format!("Enabled tool '{}'", self.name),
        )
        .into())
    }
}
