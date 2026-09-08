use clap::Args;
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

use super::support::sorted_profile_names;

#[derive(Args)]
pub struct PolicyListArgs {
    #[arg(long)]
    pub json: bool,
}

impl Execute for PolicyListArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let defs = runtime.list_policy_defs()?;
        let values: Vec<Value> = defs
            .iter()
            .map(|d| {
                json!({
                    "name": d.name,
                    "description": d.description,
                    "fs_profiles": sorted_profile_names(d),
                    "created_at": d.created_at.map(|t| t.to_rfc3339()),
                    "updated_at": d.updated_at.map(|t| t.to_rfc3339()),
                })
            })
            .collect();

        use crate::output::table::{Column, Table};
        // `orbit policy show <name>` prints a definition in full.
        let mut table = Table::new(vec![
            Column::new("NAME").fixed(),
            Column::new("DESCRIPTION"),
            Column::new("FSPROFILES"),
            Column::new("UPDATED").fixed(),
        ])
        .empty_message("no policy definitions found");
        for def in &defs {
            table.add_row(vec![
                def.name.clone(),
                def.description.clone().unwrap_or_else(|| "-".to_string()),
                sorted_profile_names(def).join(", "),
                def.updated_at
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "-".to_string()),
            ]);
        }
        Ok(Payload::list(values, table).into())
    }
}
