use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute, Payload};
use crate::output::color::Domain;

use super::support::plugin_record;

#[derive(Args)]
pub struct PluginListArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for PluginListArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let plugins = runtime.list_plugins()?;
        let records = plugins.iter().map(plugin_record).collect::<Vec<_>>();

        use crate::output::table::{Column, Table};
        use comfy_table::Cell;
        let mut table = Table::new(vec![
            Column::new("NAME").fixed(),
            Column::new("VERSION").fixed(),
            Column::new("STATUS").fixed(),
            Column::new("TOOLS").fixed(),
            Column::new("DETAILS"),
        ])
        .empty_message("no plugins installed or pinned");
        for plugin in &plugins {
            table.add_row(vec![
                Cell::new(&plugin.name),
                Cell::new(&plugin.version),
                crate::output::color::cell(plugin.status.as_str(), Domain::JobState),
                Cell::new(plugin.tools.len().to_string()),
                Cell::new(plugin.diagnostic.clone().unwrap_or_default()),
            ]);
        }
        Ok(Payload::list(records, table).into())
    }
}
