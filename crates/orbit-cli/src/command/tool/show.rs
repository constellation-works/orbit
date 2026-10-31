use clap::Args;
use orbit_core::OrbitRuntime;
use serde_json::json;

use crate::command::{Block, CommandOut, Execute, Payload};

use super::support::tool_status;

#[derive(Args)]
pub struct ToolShowArgs {
    /// Tool name
    pub name: String,
}

impl Execute for ToolShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let tool = runtime.show_tool(&self.name)?;

        use crate::output::color::{Domain, bold, text};
        let doc = json!({
            "name": tool.name,
            "description": tool.description,
            "enabled": tool.enabled,
            "active": tool.active,
            "status": tool_status(&tool),
            "builtin": tool.builtin,
            "parameters": &tool.parameters,
            "mcp_toolset": tool.mcp_toolset,
        });

        let mut header = format!(
            "{} {}\n{} {}\n{} {}\n{} {}",
            bold("Name:"),
            tool.name,
            bold("Description:"),
            tool.description,
            bold("Builtin:"),
            if tool.builtin { "yes" } else { "no" },
            bold("Status:"),
            text(tool_status(&tool), Domain::JobState)
        );
        if let Some(toolset) = tool.mcp_toolset {
            header.push_str(&format!("\n{} {toolset}", bold("MCP toolset:")));
        }

        if tool.parameters.is_empty() {
            header.push_str(&format!("\n{} (none)", bold("Parameters:")));
            return Ok(Payload::detail(doc, header).into());
        }

        header.push_str(&format!("\n{}", bold("Parameters:")));
        // A parameter schema, not a result set: keep every column even when
        // every parameter happens to share a type or be required.
        let mut table =
            crate::output::table::build_table(&["NAME", "TYPE", "REQUIRED", "DESCRIPTION"])
                .keep_all_columns();
        for p in &tool.parameters {
            let req = if p.required { "required" } else { "optional" };
            table.add_row(vec![
                p.name.clone(),
                p.param_type.clone(),
                req.to_string(),
                p.description.clone(),
            ]);
        }
        Ok(Payload::blocks(doc, vec![Block::text(header), Block::table(table)]).into())
    }
}
