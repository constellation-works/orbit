use clap::Args;
use orbit_config::CONFIG_KEY_REGISTRY;
use orbit_core::OrbitRuntime;
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct ConfigKeysArgs {
    #[arg(long)]
    pub json: bool,
}

impl Execute for ConfigKeysArgs {
    fn execute(self, _runtime: &OrbitRuntime) -> CommandOut {
        let keys: Vec<_> = CONFIG_KEY_REGISTRY
            .iter()
            .map(|entry| {
                json!({
                    "key": entry.key,
                    "type": entry.value_type,
                    "description": entry.description,
                })
            })
            .collect();
        let text = CONFIG_KEY_REGISTRY
            .iter()
            .map(|entry| {
                format!(
                    "{:<36}  {:<24}  {}",
                    entry.key, entry.value_type, entry.description
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(Payload::detail(json!({ "keys": keys }), text).into())
    }
}
