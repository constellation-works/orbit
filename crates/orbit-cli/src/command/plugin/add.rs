use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::PluginAddOptions;

use crate::command::{CommandOut, Execute, Payload};

use super::support::plugin_record;

#[derive(Args)]
pub struct PluginAddArgs {
    /// Plugin source: a directory, a `git+<url>#<ref>` reference, or a tar archive
    pub source: String,
    /// Replace an existing install of the same version
    #[arg(long)]
    pub force: bool,
    /// Enable the plugin as part of the install
    #[arg(long)]
    pub enable: bool,
    /// Permission grants to record when enabling (repeatable, comma-separated)
    #[arg(long = "grant", value_delimiter = ',')]
    pub grants: Vec<String>,
}

impl Execute for PluginAddArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let summary = runtime.add_plugin(
            &self.source,
            &PluginAddOptions {
                force: self.force,
                enable: self.enable,
                grants: self.grants,
            },
        )?;
        let mut text = format!(
            "Installed plugin '{}' v{} to {}",
            summary.name, summary.version, summary.install_path
        );
        if summary.tools.is_empty() {
            text.push_str("\nNo tools were registered; run `orbit plugin show` for details.");
        } else {
            text.push_str("\nTools:");
            for tool in &summary.tools {
                text.push_str(&format!("\n  {}", tool.name));
            }
        }
        if !self.enable {
            text.push_str(&format!(
                "\n\nNext step:\n  orbit plugin enable {}",
                summary.name
            ));
        }
        Ok(Payload::detail(plugin_record(&summary), text).into())
    }
}
