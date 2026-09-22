use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::{
    PluginPermissionChange, PluginUpgradeOptions, PluginUpgradeResult,
};

use crate::command::{CommandOut, Execute, Payload};

use super::support::plugin_record;

#[derive(Args)]
pub struct PluginUpgradeArgs {
    /// Installed plugin namespace
    pub name: String,
    /// Replacement source; defaults to the source recorded at install time
    pub source: Option<String>,
    /// Complete permission grant set authorizing and enabling the new manifest
    /// (repeatable, comma-separated): fs, network, env_pass, orbit_tools,
    /// unsandboxed. Without it, widened requests disable the plugin and clear
    /// its grants.
    #[arg(long = "grant", value_delimiter = ',')]
    pub grants: Vec<String>,
}

impl Execute for PluginUpgradeArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let result = runtime.upgrade_plugin(
            &self.name,
            self.source.as_deref(),
            &PluginUpgradeOptions {
                grants: self.grants,
            },
        )?;
        let text = upgrade_text(&result);
        let mut doc = plugin_record(&result.summary);
        doc["permission_changes"] = serde_json::Value::Array(
            result
                .permission_changes
                .iter()
                .map(|change| {
                    serde_json::json!({
                        "grant": change.grant.as_str(),
                        "previous": change.previous,
                        "requested": change.requested,
                        "widened": change.widened,
                    })
                })
                .collect(),
        );
        doc["grants_reset"] = serde_json::json!(result.grants_reset);
        Ok(Payload::detail(doc, text).into())
    }
}

pub(super) fn upgrade_text(result: &PluginUpgradeResult) -> String {
    let summary = &result.summary;
    let mut text = format!(
        "Upgraded plugin '{}' to v{} at {}\nRequested permissions:",
        summary.name, summary.version, summary.install_path
    );
    if result.permission_changes.is_empty() {
        text.push_str(" unchanged");
    } else {
        for change in &result.permission_changes {
            text.push_str(&format_permission_change(change));
        }
    }
    if let Some(diagnostic) = &summary.diagnostic {
        text.push_str(&format!("\n\n{diagnostic}"));
    }
    text
}

pub(super) fn format_permission_change(change: &PluginPermissionChange) -> String {
    format!(
        "\n  {}: {} -> {}{}",
        change.grant,
        change.previous.as_deref().unwrap_or("(not requested)"),
        change.requested.as_deref().unwrap_or("(not requested)"),
        if change.widened { " (widened)" } else { "" }
    )
}
