use clap::Args;
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};

use orbit_core::OrbitError;
use orbit_types::tool::{McpRole, McpToolset, McpToolsetSelection};

use crate::command::{Block, CommandOut, Execute, Payload};
use crate::output::color::Domain;

use super::support::{format_required_tool_input_summary, tool_status};

#[derive(Args)]
pub struct ToolListArgs {
    /// Include inactive tools that are hidden from the default agent surface
    #[arg(long)]
    pub all: bool,
    /// List only the tools an `orbit mcp serve --role <ROLE>` server
    /// advertises, with the role's tool count and approximate token size:
    /// `operator`, `manager`, `resident`, `agent` or `reader`.
    #[arg(long, value_name = "ROLE")]
    pub role: Option<String>,
}

impl Execute for ToolListArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let mut tools = if self.all {
            runtime.list_all_tools()?
        } else {
            runtime.list_tools()?
        };
        let role = self
            .role
            .as_deref()
            .map(|name| {
                name.parse::<McpRole>()
                    .map_err(|error| OrbitError::InvalidInput(format!("--role: {error}")))
            })
            .transpose()?;
        if let Some(role) = role {
            tools.retain(|tool| {
                tool.mcp_toolset
                    .is_some_and(|toolset| toolset != McpToolset::Local && role.includes(toolset))
            });
        }

        let records: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "enabled": t.enabled,
                    "active": t.active,
                    "status": tool_status(t),
                    "builtin": t.builtin,
                    "parameters": &t.parameters,
                    "mcp_toolset": t.mcp_toolset,
                })
            })
            .collect();

        use crate::output::table::Column;
        // `orbit tool show <name>` prints the untruncated description and
        // input schema for any row truncated here.
        let mut table = crate::output::table::Table::new(vec![
            Column::new("NAME").fixed(),
            Column::new("STATUS").fixed(),
            Column::new("BUILTIN").fixed(),
            Column::new("REQUIRED INPUT"),
            Column::new("DESCRIPTION"),
        ])
        .empty_message(if self.all {
            "no tools registered"
        } else {
            "no active tools (retry with --all to include inactive tools)"
        });
        for tool in &tools {
            use comfy_table::Cell;
            table.add_row(vec![
                Cell::new(&tool.name),
                crate::output::color::cell(tool_status(tool), Domain::JobState),
                Cell::new(if tool.builtin { "yes" } else { "no" }),
                Cell::new(format_required_tool_input_summary(&tool.parameters)),
                Cell::new(&tool.description),
            ]);
        }
        let Some(role) = role else {
            return Ok(Payload::list(records, table).into());
        };
        let surface = orbit_mcp::mcp_surface_report(&McpToolsetSelection::for_role(role))?;
        let summary = format!(
            "role {role}: {} builtin MCP tools, about {} tokens (toolsets: {})",
            surface.tools,
            surface.approx_tokens,
            surface
                .toolsets
                .iter()
                .map(|toolset| toolset.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let doc = json!({
            "role": role,
            "toolsets": surface.toolsets,
            "surface": {
                "tools": surface.tools,
                "approx_tokens": surface.approx_tokens,
            },
            "tools": records,
        });
        Ok(Payload::blocks(doc, vec![Block::text(summary), Block::table(table)]).into())
    }
}
