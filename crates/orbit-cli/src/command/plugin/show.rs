use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::{Block, CommandOut, Execute, Payload};

use super::support::{host_state, plugin_record, state_reason};

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
        // STATUS is the effective state in the selected workspace; the host
        // row and the reason say which layer decided it.
        header.push_str(&format!("\n{} {}", bold("Host:"), host_state(&plugin)));
        let toggle = match plugin.workspace_toggle {
            Some(true) => "on",
            Some(false) => "off",
            None => "inherits the host",
        };
        header.push_str(&format!("\n{} {toggle}", bold("Workspace toggle:")));
        if let Some(reason) = state_reason(&plugin).filter(|_| plugin.diagnostic.is_none()) {
            header.push_str(&format!("\n{} {reason}", bold("Reason:")));
        }
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
        if let Some(version) = &plugin.certified_orbit_version {
            header.push_str(&format!("\n{} {version}", bold("Certified for:")));
        }
        if plugin.unsandboxed {
            header.push_str(&format!(
                "\n{} yes (backend.sandbox: none, granted)",
                bold("Unsandboxed:")
            ));
        }
        if let Some(diagnostic) = &plugin.diagnostic {
            header.push_str(&format!("\n{} {diagnostic}", bold("Diagnostic:")));
        }
        let state_visible = runtime.ensure_plugin_state_visible().is_ok();
        if !state_visible {
            header.push_str(&format!(
                "\n{} not visible from an agent sandbox",
                bold("State and secrets:")
            ));
        }

        let mut doc = plugin_record(&plugin);
        doc["state_and_secrets_visible"] = state_visible.into();
        let mut blocks = vec![Block::text(header)];

        // Requested versus granted side by side: the manifest asks, the
        // operator grants, and only the second is authority.
        if !plugin.permissions.is_empty() {
            blocks.push(Block::text(bold("Permissions:")));
            let mut permissions =
                crate::output::table::build_table(&["GRANT", "REQUESTED", "GRANTED"])
                    .keep_all_columns();
            for permission in &plugin.permissions {
                permissions.add_row(vec![
                    permission.grant.as_str().to_string(),
                    permission
                        .requested
                        .clone()
                        .unwrap_or_else(|| "-".to_string()),
                    match (permission.granted, &permission.granted_roots) {
                        // The delta an operator needs to see: the manifest
                        // asked for the REQUESTED column, this host allowed
                        // only these roots, and the sandbox opens the overlap.
                        (true, Some(roots)) => format!("yes ({})", roots.join(", ")),
                        (true, None) => "yes".to_string(),
                        (false, _) => "no".to_string(),
                    },
                ]);
            }
            blocks.push(Block::table(permissions));
        }

        // What each declared program resolved to when the plugin was
        // enabled: the sandbox grants that path, never the caller's `PATH`.
        if !plugin.programs.is_empty() {
            blocks.push(Block::text(bold("Programs:")));
            let mut programs =
                crate::output::table::build_table(&["PROGRAM", "RESOLVED PATH", "GRANTED"])
                    .keep_all_columns();
            for program in &plugin.programs {
                programs.add_row(vec![
                    program.name.clone(),
                    program.path.as_ref().map_or_else(
                        || "-".to_string(),
                        |path| path.to_string_lossy().into_owned(),
                    ),
                    match &program.problem {
                        None => "yes".to_string(),
                        Some(problem) => format!("no ({problem})"),
                    },
                ]);
            }
            blocks.push(Block::table(programs));
        }

        if !plugin.panels.is_empty() || !plugin.links.is_empty() {
            blocks.push(Block::text(bold("Dashboard:")));
            let mut dashboard =
                crate::output::table::build_table(&["KIND", "NAME", "SOURCE"]).keep_all_columns();
            for panel in &plugin.panels {
                dashboard.add_row(vec![
                    format!("panel ({})", panel.render.as_str()),
                    panel.title.clone(),
                    panel.tool.clone(),
                ]);
            }
            for link in &plugin.links {
                dashboard.add_row(vec![
                    "link".to_string(),
                    link.title.clone(),
                    link.url.clone(),
                ]);
            }
            blocks.push(Block::table(dashboard));
        }

        if plugin.tools.is_empty() {
            blocks.push(Block::text(format!("{} (none)", bold("Tools:"))));
            return Ok(Payload::blocks(doc, blocks).into());
        }
        blocks.push(Block::text(bold("Tools:")));
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
        blocks.push(Block::table(table));
        Ok(Payload::blocks(doc, blocks).into())
    }
}
