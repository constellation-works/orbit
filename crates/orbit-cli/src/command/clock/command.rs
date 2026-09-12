use std::path::Path;

use clap::{Args, Subcommand};
use orbit_core::application::routines::{
    clock_status, inspect_clock_unit, set_clock_cadence, set_clock_enabled,
};
use orbit_core::{OrbitError, OrbitRuntime};

use super::ClockTickArgs;
use crate::command::{CommandOut, CommandOutput};
use orbit_registry::workspace_registry;

#[derive(Args)]
#[command(
    about = "Inspect and control the host scheduler clock",
    arg_required_else_help = true,
    subcommand_required = true,
    after_help = "The host-wide OS clock invokes `orbit clock tick`; it does not change `orbit routine pause <name>` state. `orbit sweep` remains a compatibility alias for `orbit clock tick`."
)]
pub struct ClockCommand {
    #[command(subcommand)]
    pub command: ClockSubcommand,
}

#[derive(Subcommand)]
pub enum ClockSubcommand {
    /// Show configured cadence and native manager state
    Status,
    /// Disable scheduled ticks; manual `orbit clock tick` remains available
    Pause,
    /// Enable scheduled ticks using the configured cadence
    Enable,
    /// Persist a whole-minute cadence and reload the installed clock unit
    Set {
        #[arg(long)]
        cadence_seconds: u64,
    },
    /// Evaluate due routines and auto-tasks on this host
    Tick(ClockTickArgs),
}

impl ClockCommand {
    pub fn execute_without_runtime(
        self,
        root_override: Option<&Path>,
        workspace_selector: Option<&str>,
    ) -> CommandOut {
        match self.command {
            ClockSubcommand::Status => {
                let global_root = selected_global_root(root_override)?;
                let status = clock_status(&global_root)?;
                let program = inspect_clock_unit()
                    .map(|inspection| inspection.status_line_suffix())
                    .unwrap_or_default();
                let state = if !status.enabled {
                    "paused"
                } else if status.schedulable {
                    "enabled"
                } else {
                    "unhealthy"
                };
                println!(
                    "clock: {} | configured cadence: {}s | effective cadence: {} | platform: {}{}",
                    state,
                    status.configured_cadence_seconds,
                    status
                        .effective_cadence_seconds
                        .map(|value| format!("{value}s"))
                        .unwrap_or_else(|| "inactive".to_string()),
                    status.platform,
                    program
                );
                if let Some(issue) = status.health_issue {
                    println!("clock health: {issue}");
                }
                Ok(CommandOutput::Silent)
            }
            ClockSubcommand::Pause => {
                let global_root = selected_global_root(root_override)?;
                let status = set_clock_enabled(&global_root, false)?;
                println!(
                    "host scheduler clock paused ({}); manual `orbit clock tick` remains available",
                    status.platform
                );
                Ok(CommandOutput::Silent)
            }
            ClockSubcommand::Enable => {
                let global_root = selected_global_root(root_override)?;
                let status = set_clock_enabled(&global_root, true)?;
                println!(
                    "host scheduler clock enabled: runs every {} seconds ({})",
                    status.configured_cadence_seconds, status.platform
                );
                Ok(CommandOutput::Silent)
            }
            ClockSubcommand::Set { cadence_seconds } => {
                let global_root = selected_global_root(root_override)?;
                set_clock_cadence(&global_root, cadence_seconds)?;
                println!(
                    "host scheduler clock cadence set to {cadence_seconds} seconds and reloaded"
                );
                Ok(CommandOutput::Silent)
            }
            ClockSubcommand::Tick(args) => {
                args.execute_without_runtime(root_override, workspace_selector)
            }
        }
    }
}

fn selected_global_root(root_override: Option<&Path>) -> Result<std::path::PathBuf, OrbitError> {
    let has_env_override = std::env::var("ORBIT_ROOT").is_ok_and(|root| !root.trim().is_empty());
    if root_override.is_some() || has_env_override {
        let cwd = std::env::current_dir().map_err(|error| OrbitError::Io(error.to_string()))?;
        return OrbitRuntime::resolve_roots_for_cwd(&cwd, root_override)
            .map(|roots| roots.global_root);
    }
    workspace_registry::global_orbit_dir()
}
