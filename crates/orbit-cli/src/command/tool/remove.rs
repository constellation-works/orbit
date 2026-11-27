use clap::Args;
use orbit_core::Core;
use orbit_core::app::tool::{self, Command};
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
pub struct ToolRemoveArgs {
    /// Tool name to remove
    pub name: String,
}

impl ExecuteCore for ToolRemoveArgs {
    fn execute(self, core: &Core) -> CommandOut {
        tool::exec(core, Command::Remove(self.name.clone()))?;
        Ok(Payload::detail(
            json!({ "tool": self.name, "removed": true }),
            format!("Removed tool '{}'", self.name),
        )
        .into())
    }
}
