use clap::Args;
use orbit_core::OrbitRuntime;
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};
use crate::output::color::Domain;

#[derive(Args)]
pub struct PluginSyncArgs {
    /// Report what would be installed without installing it
    #[arg(long)]
    pub dry_run: bool,
    /// Complete permission grant set consenting to enable pinned plugins whose
    /// manifests request grants (repeatable, comma-separated)
    #[arg(long = "grant", value_delimiter = ',', conflicts_with = "dry_run")]
    pub grants: Vec<String>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for PluginSyncArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let outcomes = runtime.sync_plugins(self.dry_run, &self.grants)?;
        let records = outcomes
            .iter()
            .map(|outcome| {
                json!({
                    "name": outcome.name,
                    "status": outcome.status.as_str(),
                    "message": outcome.message,
                })
            })
            .collect::<Vec<_>>();

        use crate::output::table::{Column, Table};
        use comfy_table::Cell;
        let mut table = Table::new(vec![
            Column::new("PLUGIN").fixed(),
            Column::new("STATUS").fixed(),
            Column::new("RESULT"),
        ])
        .empty_message("this workspace pins no plugins (.orbit/plugins.yaml)");
        for outcome in &outcomes {
            table.add_row(vec![
                Cell::new(&outcome.name),
                crate::output::color::cell(outcome.status.as_str(), Domain::JobState),
                Cell::new(&outcome.message),
            ]);
        }
        Ok(Payload::list(records, table).into())
    }
}
