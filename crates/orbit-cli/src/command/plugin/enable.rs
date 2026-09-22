use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::PluginEnableOptions;

use crate::command::{CommandOut, Execute, Payload};

use super::support::{append_enable_report_text, enable_report_json, plugin_record};

#[derive(Args)]
pub struct PluginEnableArgs {
    /// Plugin namespace
    pub name: String,
    /// Complete permission grant set to record (repeatable, comma-separated):
    /// fs, network, env_pass, orbit_tools, unsandboxed. Replaces the recorded
    /// set when present; omitting --grant preserves it. A tool whose plugin
    /// requests a grant it has not been given registers inactive.
    #[arg(long = "grant", value_delimiter = ',')]
    pub grants: Vec<String>,
    /// Overwrite a seeded routine or auto-task that was edited after Orbit
    /// wrote it. Without this, a customised file is preserved with a warning.
    #[arg(long)]
    pub force: bool,
}

impl Execute for PluginEnableArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let options = PluginEnableOptions {
            grants: self.grants,
            force: self.force,
        };
        let result = runtime.enable_plugin(&self.name, &options)?;
        let summary = &result.summary;
        let mut text = format!(
            "Enabled plugin '{}' v{}. Its tools register on the next Orbit command.",
            summary.name, summary.version
        );
        if let Some(diagnostic) = &summary.diagnostic {
            text.push_str(&format!("\n\nPlugin is inactive: {diagnostic}"));
        }
        append_enable_report_text(&mut text, &result.seeded, &result.skills, &result.warnings);

        let mut doc = plugin_record(summary);
        enable_report_json(&mut doc, &result.seeded, &result.skills, &result.warnings);
        Ok(Payload::detail(doc, text).into())
    }
}
