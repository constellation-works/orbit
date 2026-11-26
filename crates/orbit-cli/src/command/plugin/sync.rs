use clap::Args;
use orbit_core::Core;
use orbit_core::app::plugin::{Command, Outcome, exec};
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};
use crate::output::common::color::Domain;

#[derive(Args)]
pub struct PluginSyncArgs {
    /// Report what would be installed without installing it
    #[arg(long)]
    pub dry_run: bool,
    /// Complete permission grant set consenting to enable pinned plugins whose
    /// manifests request grants (repeatable, comma-separated)
    #[arg(long = "grant", value_delimiter = ',', conflicts_with = "dry_run")]
    pub grants: Vec<String>,
}

impl ExecuteCore for PluginSyncArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Synced(outcomes) = exec(
            core,
            Command::Sync {
                dry_run: self.dry_run,
                grants: self.grants,
            },
        )?
        else {
            unreachable!("plugin command returned an unrelated outcome");
        };
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

        use crate::output::common::table::{Column, Table};
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
                crate::output::common::color::cell(outcome.status.as_str(), Domain::JobState),
                Cell::new(&outcome.message),
            ]);
        }
        Ok(Payload::list(records, table).into())
    }
}
