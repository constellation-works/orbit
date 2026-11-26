use clap::Args;
use orbit_core::Core;
use orbit_core::app::run::{Command, exec};
use serde_json::Value;

use super::support::{script_status_json, short_digest};
use crate::command::{CommandOut, Payload};
use crate::output::common::table::{Column, Table};

#[derive(Args)]
pub struct ScriptListArgs {}

impl ScriptListArgs {
    pub(super) fn execute(self, core: &Core) -> CommandOut {
        let statuses = exec(core, Command::scripts())?.value;
        let records: Vec<Value> = statuses.iter().map(script_status_json).collect();
        let mut table = Table::new(vec![
            Column::new("NAME").fixed(),
            Column::new("ENTRY"),
            Column::new("GRANT").fixed(),
            Column::new("DIGEST"),
        ])
        .empty_message(
            "no scripts in this workspace (manifests live in .orbit/resources/scripts/)",
        );
        for status in &statuses {
            table.add_row(vec![
                status.name.clone(),
                status
                    .asset
                    .as_ref()
                    .map(|asset| asset.spec.entry.clone())
                    .unwrap_or_else(|| "-".to_string()),
                status.state.to_string(),
                status
                    .asset
                    .as_ref()
                    .map(|asset| short_digest(&asset.digest))
                    .or_else(|| {
                        status
                            .grant
                            .as_ref()
                            .map(|grant| short_digest(&grant.digest))
                    })
                    .unwrap_or_else(|| "-".to_string()),
            ]);
        }
        Ok(Payload::list(records, table).into())
    }
}
