use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute, Payload};

use super::support::plugin_record;

#[derive(Args)]
pub struct PluginEnableArgs {
    /// Plugin namespace
    pub name: String,
    /// Permission grants to record (repeatable, comma-separated). Grants are
    /// recorded now and enforced by a later Orbit release.
    #[arg(long = "grant", value_delimiter = ',')]
    pub grants: Vec<String>,
}

impl Execute for PluginEnableArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let summary = runtime.enable_plugin(&self.name, &self.grants)?;
        let text = format!(
            "Enabled plugin '{}' v{}. Its tools register on the next Orbit command.",
            summary.name, summary.version
        );
        Ok(Payload::detail(plugin_record(&summary), text).into())
    }
}
