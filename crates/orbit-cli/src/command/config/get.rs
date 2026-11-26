use clap::Args;
use orbit_core::Core;
use orbit_core::app::config::{Command, ConfigGetResult, Outcome, exec};
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};

use super::support::ConfigScopeArg;

#[derive(Args)]
pub struct ConfigGetArgs {
    /// Dotted config.toml key, e.g. workflow.base_branch
    pub key: String,
    /// Select the layered effective config or resolve one file in isolation,
    /// including built-in defaults for keys that file omits.
    #[arg(long, value_enum, default_value_t = ConfigScopeArg::Effective)]
    pub scope: ConfigScopeArg,
}

impl ExecuteCore for ConfigGetArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Got(result) = exec(
            core,
            Command::Get {
                key: self.key.clone(),
                scope: self.scope.into(),
            },
        )?
        else {
            unreachable!("config get returned an unrelated outcome");
        };
        match result {
            ConfigGetResult::Effective { value } => {
                let text = format_value_for_display(&value);
                Ok(Payload::detail(
                    json!({
                        "key": self.key,
                        "scope": "effective",
                        "value": value,
                    }),
                    text,
                )
                .into())
            }
            ConfigGetResult::Scoped {
                scope,
                path,
                value,
                exists,
            } => {
                let path = match path {
                    Some(path) => serde_json::Value::String(path.to_string_lossy().into_owned()),
                    None => serde_json::Value::Null,
                };
                let text = format_value_for_display(&value);
                Ok(Payload::detail(
                    json!({
                        "key": self.key,
                        "scope": scope,
                        "path": path,
                        "value": value,
                        "exists": exists,
                    }),
                    text,
                )
                .into())
            }
        }
    }
}

fn format_value_for_display(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}
