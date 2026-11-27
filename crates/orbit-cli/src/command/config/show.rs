use clap::Args;
use orbit_core::Core;
use orbit_core::app::config::{Command, Outcome, exec};

use super::support::ConfigScopeArg;
use crate::command::{CommandOut, ExecuteCore};

#[derive(Args)]
pub struct ConfigShowArgs {
    /// Select the layered effective config or resolve one file in isolation,
    /// including built-in defaults for keys that file omits.
    #[arg(long, value_enum, default_value_t = ConfigScopeArg::Effective)]
    pub scope: ConfigScopeArg,
    /// List every key, including unconfigured advanced keys and sections
    /// whose keys are all unset, plus the Paths, Crew purposes and Review
    /// summaries.
    #[arg(long, visible_alias = "advanced")]
    pub all: bool,
}

impl ExecuteCore for ConfigShowArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Shown(shown) = exec(
            core,
            Command::Show {
                scope: self.scope.into(),
            },
        )?
        else {
            unreachable!("config show returned an unrelated outcome");
        };
        Ok(crate::output::config::show_payload(*shown, self.all).into())
    }
}
