use clap::Args;
use orbit_core::Core;
use orbit_core::app::tool::{self, Command};
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
pub struct ToolDisableArgs {
    /// Tool name to disable
    pub name: String,
}

impl ExecuteCore for ToolDisableArgs {
    fn execute(self, core: &Core) -> CommandOut {
        tool::exec(core, Command::Disable(self.name.clone()))?;
        Ok(Payload::detail(
            json!({ "tool": self.name, "enabled": false }),
            format!("Disabled tool '{}'", self.name),
        )
        .into())
    }
}
