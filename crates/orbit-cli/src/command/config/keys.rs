use clap::Args;
use orbit_core::Core;
use orbit_core::app::config::{Command, Outcome, exec};

use crate::command::{CommandOut, ExecuteCore};

#[derive(Args)]
pub struct ConfigKeysArgs {}

impl ExecuteCore for ConfigKeysArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Keys(_) = exec(core, Command::Keys)? else {
            unreachable!("config keys returned an unrelated outcome");
        };
        Ok(crate::output::config::keys_payload().into())
    }
}
