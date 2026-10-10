use std::path::Path;

use clap::{Args, ValueEnum};
use orbit_core::{OrbitError, OrbitRuntime, ShipMode};
use orbit_registry::workspace_registry;
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

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

impl Execute for WorkspaceShipModeArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let workspace_id = runtime
            .workspace_runtime_binding()
            .map(|binding| binding.logical_workspace_id.clone())
            .ok_or_else(|| {
                OrbitError::WorkspaceError(
                    "this checkout is not a registered workspace; run `orbit workspace init` \
                     first, or select one with `--workspace`"
                        .to_string(),
                )
            })?;
        let registry_path = workspace_registry::registry_path_for(&runtime.global_root());
        let Some(mode) = self.mode else {
            let registry = workspace_registry::load_registry_from(&registry_path)?;
            let workspace = workspace_registry::find_workspace_by_id(&registry, &workspace_id)
                .ok_or_else(|| {
                    OrbitError::WorkspaceError(format!("unknown workspace '{workspace_id}'"))
                })?;
            let mode = orbit_core::resolved_ship_mode(workspace).as_input_value();
            return Ok(Payload::detail(
                json!({ "workspace_id": workspace_id, "ship_mode": mode }),
                mode.to_string(),
            )
            .into());
        };
        let outcome = rebind_at_registry_path(&registry_path, &workspace_id, mode.into())?;
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

fn rebind_at_registry_path(
    registry_path: &Path,
    workspace_id: &str,
    mode: ShipMode,
) -> Result<workspace_registry::WorkspaceShipModeRebind, OrbitError> {
    workspace_registry::with_registry_lock(registry_path, || {
        let mut registry = workspace_registry::load_registry_from(registry_path)?;
        let outcome =
            workspace_registry::rebind_workspace_ship_mode(&mut registry, workspace_id, mode)?;
        if outcome.changed {
            workspace_registry::save_registry_to(&registry, registry_path)?;
        }
        Ok(outcome)
    })
}
