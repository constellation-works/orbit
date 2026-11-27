use clap::Args;
use orbit_core::Core;
use orbit_core::app::config::{Command, Outcome, exec};

use crate::command::{CommandOut, ExecuteCore};

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
        Ok(crate::output::config::path_payload(&path).into())
    }
}
