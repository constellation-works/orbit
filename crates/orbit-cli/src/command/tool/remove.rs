use clap::Args;
use orbit_core::OrbitRuntime;

use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct ToolRemoveArgs {
    /// Tool name to remove
    pub name: String,
}

impl Execute for ToolRemoveArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        runtime.remove_tool(&self.name)?;
        Ok(Payload::detail(
            json!({ "tool": self.name, "removed": true }),
            format!("Removed tool '{}'", self.name),
        )
        .into())
    }
}
