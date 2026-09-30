use clap::Args;
use orbit_core::OrbitRuntime;

use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

use super::support::global_config_path;

#[derive(Args)]
pub struct ConfigPathArgs {
    /// Print the global config.toml path instead of the effective one
    #[arg(long)]
    pub global: bool,
}

impl Execute for ConfigPathArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let path = if self.global {
            global_config_path(runtime)
        } else {
            runtime.config_path()?
        };
        let path = path.to_string_lossy().into_owned();
        Ok(Payload::detail(json!({ "path": path }), path.clone()).into())
    }
}
