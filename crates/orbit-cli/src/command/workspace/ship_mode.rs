use clap::{Args, ValueEnum};
use orbit_core::Core;
use orbit_core::app::run::ShipMode;
use orbit_core::app::workspace::{Command, Outcome, exec};
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Clone, Copy, ValueEnum)]
pub enum CliShipMode {
    Pr,
    Local,
}

impl From<CliShipMode> for ShipMode {
    fn from(mode: CliShipMode) -> Self {
        match mode {
            CliShipMode::Pr => ShipMode::Pr,
            CliShipMode::Local => ShipMode::Local,
        }
    }
}

#[derive(Args)]
#[command(
    about = "Show or rebind how this workspace delivers shipped tasks",
    after_long_help = "With no MODE, prints the registered mode. With one, rebinds it in the workspace registry\nand reports the previous mode; managed defaults and config files are left untouched.\n\n  pr     open a pull request (needs a Git remote on a forge host)\n  local  merge into the local base branch without a pull request\n\nExamples:\n  orbit workspace ship-mode\n  orbit workspace ship-mode local\n  orbit --workspace <NAME> workspace ship-mode pr --json"
)]
pub struct WorkspaceShipModeArgs {
    /// New ship mode. Omit to print the current one.
    #[arg(value_enum, value_name = "MODE")]
    mode: Option<CliShipMode>,
}

impl ExecuteCore for WorkspaceShipModeArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let outcome = match exec(core, Command::ShipMode(self.mode.map(Into::into)))? {
            Outcome::ShipMode { workspace_id, mode } => {
                let mode = mode.as_input_value();
                return Ok(Payload::detail(
                    json!({ "workspace_id": workspace_id, "ship_mode": mode }),
                    mode.to_string(),
                )
                .into());
            }
            Outcome::ShipModeRebound(outcome) => outcome,
            _ => unreachable!("workspace ship-mode returned an unrelated outcome"),
        };
        let action = if outcome.changed {
            "rebound"
        } else {
            "unchanged"
        };
        let previous = outcome.previous.as_input_value();
        let ship_mode = outcome.ship_mode.as_input_value();
        Ok(Payload::detail(
            json!({
                "action": action,
                "workspace_id": outcome.workspace_id,
                "ship_mode": ship_mode,
                "previous": previous,
                "changed": outcome.changed,
            }),
            format!(
                "ship mode {action} for workspace '{}'\nprevious: {previous}\ncurrent:  {ship_mode}",
                outcome.workspace_id
            ),
        )
        .into())
    }
}
