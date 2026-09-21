use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute, Payload};

use super::support::plugin_record;

#[derive(Args)]
pub struct PluginDisableArgs {
    /// Plugin namespace
    pub name: String,
}

impl Execute for PluginDisableArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let summary = runtime.disable_plugin(&self.name)?;
        let text = format!(
            "Disabled plugin '{}'. Its tools leave the tool surface on the next Orbit command.",
            summary.name
        );
        Ok(Payload::detail(plugin_record(&summary), text).into())
    }
}
