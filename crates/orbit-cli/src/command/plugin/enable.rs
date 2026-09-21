use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::PluginEnableOptions;

use crate::command::{CommandOut, Execute, Payload};

use super::support::plugin_record;

#[derive(Args)]
pub struct PluginEnableArgs {
    /// Plugin namespace
    pub name: String,
    /// Permission grants to record (repeatable, comma-separated): fs, network,
    /// env_pass, orbit_tools, unsandboxed. A tool whose plugin requests a
    /// grant it has not been given registers inactive.
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
        for seeded in &result.seeded {
            text.push_str(&format!(
                "\n  {} {} {} ({})",
                seeded.kind,
                seeded.name,
                seeded.action.as_str(),
                seeded.path.display()
            ));
        }
        if !result.seeded.is_empty() {
            text.push_str(
                "\n  Seeded schedules are disabled; review one, then set `enabled: true` to run it.",
            );
        }
        for link in &result.skills {
            text.push_str(&format!(
                "\n  skill {} linked at {}",
                link.skill_id,
                link.link.display()
            ));
        }
        for warning in &result.warnings {
            text.push_str(&format!("\n  warning: {warning}"));
        }

        let mut doc = plugin_record(summary);
        doc["seeded"] = serde_json::Value::Array(
            result
                .seeded
                .iter()
                .map(|seeded| {
                    serde_json::json!({
                        "kind": seeded.kind,
                        "name": seeded.name,
                        "path": seeded.path.display().to_string(),
                        "action": seeded.action.as_str(),
                        "warning": seeded.warning,
                    })
                })
                .collect(),
        );
        doc["skills"] = serde_json::Value::Array(
            result
                .skills
                .iter()
                .map(|link| {
                    serde_json::json!({
                        "skill_id": link.skill_id,
                        "link": link.link.display().to_string(),
                        "target": link.target.display().to_string(),
                    })
                })
                .collect(),
        );
        doc["warnings"] = serde_json::json!(result.warnings);
        Ok(Payload::detail(doc, text).into())
    }
}
