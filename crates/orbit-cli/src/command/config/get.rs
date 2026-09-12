use clap::Args;
use orbit_config::{admit_config_key, load_effective_config};
use orbit_core::OrbitRuntime;
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

use super::support::{ConfigScopeArg, open_store_for_scope, runtime_config_roots};

#[derive(Args)]
pub struct ConfigGetArgs {
    /// Dotted config.toml key, e.g. workflow.base_branch
    pub key: String,
    #[arg(long, value_enum, default_value_t = ConfigScopeArg::Effective)]
    pub scope: ConfigScopeArg,
    #[arg(long)]
    pub json: bool,
}

impl Execute for ConfigGetArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if self.scope == ConfigScopeArg::Effective {
            admit_config_key(&self.key)?;
            let effective = load_effective_config(&runtime_config_roots(runtime))?;
            let value = effective
                .value_for(&self.key)
                .unwrap_or(serde_json::Value::Null);
            let text = format_value_for_display(&value);
            return Ok(Payload::detail(
                json!({
                    "key": self.key,
                    "scope": "effective",
                    "value": value,
                }),
                text,
            )
            .into());
        }

        let store = open_store_for_scope(runtime, self.scope)?;
        admit_config_key(&self.key)?;
        let file_exists = store.exists_on_disk();
        let (value, exists) = if file_exists {
            match store.explicit_value(&self.key)? {
                Some(value) => (value, true),
                None => (serde_json::Value::Null, false),
            }
        } else {
            (serde_json::Value::Null, false)
        };
        let path = if file_exists {
            serde_json::Value::String(store.path().to_string_lossy().into_owned())
        } else {
            serde_json::Value::Null
        };
        let text = format_value_for_display(&value);
        Ok(Payload::detail(
            json!({
                "key": self.key,
                "scope": store.scope().label(),
                "path": path,
                "value": value,
                "exists": exists,
            }),
            text,
        )
        .into())
    }
}

fn format_value_for_display(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}
