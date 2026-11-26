use clap::Args;
use orbit_config::CONFIG_KEY_REGISTRY;
use orbit_core::Core;
use orbit_core::app::config::{Command, Outcome, exec};
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};
use crate::output::common::table::{Column, Table};

#[derive(Args)]
pub struct ConfigKeysArgs {}

impl ExecuteCore for ConfigKeysArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Keys(_) = exec(core, Command::Keys)? else {
            unreachable!("config keys returned an unrelated outcome");
        };
        let keys: Vec<_> = CONFIG_KEY_REGISTRY
            .iter()
            .map(|entry| {
                json!({
                    "key": entry.key,
                    "type": entry.value_type,
                    "section": entry.section.token(),
                    "description": entry.description,
                    "detail": entry.detail,
                    "allowed": entry.allowed.to_json(),
                })
            })
            .collect();
        let mut table = Table::new(vec![
            Column::new("KEY").fixed(),
            Column::new("TYPE").fixed(),
            Column::new("SECTION").fixed(),
            Column::new("DESCRIPTION"),
            Column::new("ALLOWED"),
        ])
        .empty_message("no config keys registered");

        for entry in CONFIG_KEY_REGISTRY.iter() {
            let allowed = entry.allowed.summary().unwrap_or_default();
            table.add_row(vec![
                entry.key,
                entry.value_type,
                entry.section.token(),
                entry.description,
                allowed.as_str(),
            ]);
        }

        Ok(Payload::enveloped_list(json!({ "keys": keys.clone() }), keys, table).into())
    }
}
