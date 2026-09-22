use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::PluginAddOptions;

use crate::command::{CommandOut, Execute, Payload};

use super::support::{append_enable_report_text, enable_report_json, plugin_record};

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
    /// Complete permission grant set to record when enabling (repeatable,
    /// comma-separated): fs, network, env_pass, orbit_tools, unsandboxed.
    /// Replaces any recorded set.
    #[arg(long = "grant", value_delimiter = ',', requires = "enable")]
    pub grants: Vec<String>,
}

impl Execute for PluginAddArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let result = runtime.add_plugin(
            &self.source,
            &PluginAddOptions {
                force: self.force,
                enable: self.enable,
                grants: self.grants,
            },
        )?;
        let summary = &result.summary;
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
        if let Some(diagnostic) = &summary.diagnostic {
            text.push_str(&format!("\n\n{diagnostic}"));
        } else if summary.status == orbit_types::plugin::PluginStatus::Disabled {
            text.push_str(&format!(
                "\n\nNext step:\n  orbit plugin enable {}",
                summary.name
            ));
        }
        // Same report `orbit plugin enable` renders, carried out of the
        // install rather than collapsed into the summary, so the seeded
        // schedules, skill links and warnings `--enable` produced here are
        // not only visible on the two-step `add` then `enable` path.
        append_enable_report_text(&mut text, &result.seeded, &result.skills, &result.warnings);

        let mut doc = plugin_record(summary);
        enable_report_json(&mut doc, &result.seeded, &result.skills, &result.warnings);
        Ok(Payload::detail(doc, text).into())
    }
}
