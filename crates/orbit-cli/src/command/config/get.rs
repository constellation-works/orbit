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
    /// Select the layered effective config or resolve one file in isolation,
    /// including built-in defaults for keys that file omits.
    #[arg(long, value_enum, default_value_t = ConfigScopeArg::Effective)]
    pub scope: ConfigScopeArg,
    /// Emit JSON. For global/workspace scope, `exists` reports whether the key
    /// is explicitly set, even when `value` contains its resolved default.
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
        let value = store.effective_value(&self.key)?;
        let exists = store.is_key_set(&self.key);
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
