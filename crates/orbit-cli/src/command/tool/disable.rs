use clap::Args;
use orbit_core::OrbitRuntime;

use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct ToolDisableArgs {
    /// Tool name to disable
    pub name: String,
}

impl Execute for ToolDisableArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        runtime.disable_tool(&self.name)?;
        Ok(Payload::detail(
            json!({ "tool": self.name, "enabled": false }),
            format!("Disabled tool '{}'", self.name),
        )
        .into())
    }
}
