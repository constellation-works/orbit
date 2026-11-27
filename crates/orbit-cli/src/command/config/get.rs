use clap::Args;
use orbit_core::Core;
use orbit_core::app::config::{Command, Outcome, exec};

use super::support::ConfigScopeArg;
use crate::command::{CommandOut, ExecuteCore};

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
        Ok(crate::output::config::get_payload(&self.key, result).into())
    }
}
