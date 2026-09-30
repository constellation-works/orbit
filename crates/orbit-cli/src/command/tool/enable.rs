use clap::Args;
use orbit_core::OrbitRuntime;

use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct ToolEnableArgs {
    /// Tool name to enable
    pub name: String,
}

impl Execute for ToolEnableArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        runtime.enable_tool(&self.name)?;
        Ok(Payload::detail(
            json!({ "tool": self.name, "enabled": true }),
            format!("Enabled tool '{}'", self.name),
        )
        .into())
    }
}
