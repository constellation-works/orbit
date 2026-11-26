use clap::Args;
use orbit_core::Core;
use orbit_core::app::config::{Command, Outcome, exec};
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
pub struct ConfigPathArgs {
    /// Print the global config.toml path instead of the effective one
    #[arg(long)]
    pub global: bool,
}

impl ExecuteCore for ConfigPathArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Path(path) = exec(
            core,
            Command::Path {
                global: self.global,
            },
        )?
        else {
            unreachable!("config path returned an unrelated outcome");
        };
        let path = path.to_string_lossy().into_owned();
        Ok(Payload::detail(json!({ "path": path }), path.clone()).into())
    }
}
