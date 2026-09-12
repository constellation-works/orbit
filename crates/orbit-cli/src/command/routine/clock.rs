use std::path::Path;

use clap::{Args, Subcommand};
use orbit_core::application::routines::{
    clock_status, inspect_clock_unit, set_clock_cadence, set_clock_enabled,
};

use crate::command::{CommandOut, CommandOutput};

#[derive(Args)]
#[command(
    after_help = "This controls the host-wide OS clock that invokes `orbit sweep`; it does not change `orbit routine pause <name>` state or prevent a manual `orbit sweep`."
)]
pub struct RoutineClockArgs {
    #[command(subcommand)]
    command: RoutineClockSubcommand,
}

#[derive(Subcommand)]
enum RoutineClockSubcommand {
    /// Show configured cadence and native manager state
    Status,
    /// Disable scheduled sweep invocations; manual `orbit sweep` remains available
    Pause,
    /// Enable scheduled sweep invocations using the configured cadence
    Enable,
    /// Persist a whole-minute cadence and reload the installed clock unit
    Set {
        #[arg(long)]
        cadence_seconds: u64,
    },
}

impl RoutineClockArgs {
    pub fn execute_without_runtime(self, global_root: &Path) -> CommandOut {
        match self.command {
            RoutineClockSubcommand::Status => {
                let status = clock_status(global_root)?;
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
            }
            RoutineClockSubcommand::Pause => {
                let status = set_clock_enabled(global_root, false)?;
                println!(
                    "host sweep clock paused ({}); manual `orbit sweep` remains available",
                    status.platform
                );
            }
            RoutineClockSubcommand::Enable => {
                let status = set_clock_enabled(global_root, true)?;
                println!(
                    "host sweep clock enabled: runs every {} seconds ({})",
                    status.configured_cadence_seconds, status.platform
                );
            }
            RoutineClockSubcommand::Set { cadence_seconds } => {
                set_clock_cadence(global_root, cadence_seconds)?;
                println!("host sweep clock cadence set to {cadence_seconds} seconds and reloaded");
            }
        }
        Ok(CommandOutput::Silent)
    }
}
