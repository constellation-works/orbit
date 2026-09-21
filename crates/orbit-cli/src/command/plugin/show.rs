use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::{Block, CommandOut, Execute, Payload};

use super::support::plugin_record;

#[derive(Args)]
pub struct PluginShowArgs {
    /// Plugin namespace
    pub name: String,
}

impl Execute for PluginShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let plugin = runtime.show_plugin(&self.name)?;

        use crate::output::color::{Domain, bold, text};
        let mut header = format!(
            "{} {}\n{} {}\n{} {}\n{} {}\n{} {}",
            bold("Name:"),
            plugin.name,
            bold("Version:"),
            plugin.version,
            bold("Status:"),
            text(plugin.status.as_str(), Domain::JobState),
            bold("Install path:"),
            plugin.install_path,
            bold("Manifest digest:"),
            plugin.manifest_digest,
        );
        if let Some(publisher) = &plugin.publisher {
            header.push_str(&format!("\n{} {publisher}", bold("Publisher:")));
        }
        if !plugin.description.trim().is_empty() {
            header.push_str(&format!(
                "\n{} {}",
                bold("Description:"),
                plugin.description
            ));
        }
        header.push_str(&format!(
            "\n{} {}",
            bold("Pinned by this workspace:"),
            if plugin.pinned { "yes" } else { "no" }
        ));
        // Requested versus granted side by side: the manifest asks, the
        // operator grants, and only the second is authority.
        header.push_str(&format!(
            "\n{} {}\n{} {}",
            bold("Requested permissions:"),
            display_list(&plugin.requested_permissions),
            bold("Granted:"),
            display_list(&plugin.granted),
        ));
        if let Some(diagnostic) = &plugin.diagnostic {
            header.push_str(&format!("\n{} {diagnostic}", bold("Diagnostic:")));
        }

        let doc = plugin_record(&plugin);
        if plugin.tools.is_empty() {
            header.push_str(&format!("\n{} (none)", bold("Tools:")));
            return Ok(Payload::detail(doc, header).into());
        }
        header.push_str(&format!("\n{}", bold("Tools:")));
        let mut table = crate::output::table::build_table(&["NAME", "MCP", "KIND", "STATUS"])
            .keep_all_columns();
        for tool in &plugin.tools {
            table.add_row(vec![
                tool.name.clone(),
                tool.advertised_name
                    .clone()
                    .unwrap_or_else(|| "(not advertised)".to_string()),
                tool.execution_kind.as_str().to_string(),
                if tool.active { "active" } else { "inactive" }.to_string(),
            ]);
        }
        Ok(Payload::blocks(doc, vec![Block::text(header), Block::table(table)]).into())
    }
}

fn display_list(values: &[String]) -> String {
    if values.is_empty() {
        "(none)".to_string()
    } else {
        values.join(", ")
    }
}
