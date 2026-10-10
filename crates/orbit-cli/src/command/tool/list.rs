use clap::Args;
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};
use crate::output::color::Domain;

use super::support::{format_required_tool_input_summary, tool_status};

#[derive(Args)]
pub struct ToolListArgs {
    /// Include inactive tools that are hidden from the default agent surface
    #[arg(long, alias = "include-hidden")]
    pub all: bool,
}

impl Execute for ToolListArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let tools = if self.all {
            runtime.list_all_tools()?
        } else {
            runtime.list_tools()?
        };

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
        Ok(Payload::list(records, table).into())
    }
}
