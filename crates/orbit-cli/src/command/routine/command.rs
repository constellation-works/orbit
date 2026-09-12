//! `orbit routine` — inspect and control routines on this host [ORB-10021].
//!
//! Every subcommand operates on the global registry and host-local scheduler
//! state, so the whole parent command dispatches without a workspace runtime
//! (see `main.rs`), like `orbit sweep` itself.

use std::path::Path;

use clap::{Args, Subcommand};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_registry::workspace_registry;

use super::clock::RoutineClockArgs;
use super::init::RoutineInitArgs;
use super::list::RoutineListArgs;
use super::pause::RoutinePauseArgs;
use super::resume::RoutineResumeArgs;
use super::show::RoutineShowArgs;
use crate::command::CommandOut;

#[derive(Args)]
#[command(
    about = "Inspect and control scheduled routines on this host",
    arg_required_else_help = true,
    subcommand_required = true,
    after_help = "Routine definitions are versioned YAML under `.orbit/routines/` in any\n\
                  registered owner checkout. Pauses are host-local and never synced.\n\
                  The scheduler pass itself is `orbit sweep`."
)]
pub struct RoutineCommand {
    #[command(subcommand)]
    pub command: RoutineSubcommand,
}

impl RoutineCommand {
    /// Resolve the selected global root once for every routine subcommand;
    /// none may bootstrap a workspace from the caller's cwd.
    pub fn execute_without_runtime(self, root_override: Option<&Path>) -> CommandOut {
        let global_root = selected_global_root(root_override)?;
        self.command.execute_without_runtime(&global_root)
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

#[derive(Subcommand)]
pub enum RoutineSubcommand {
    /// List every routine with toggles, next-due, and last fire
    List(RoutineListArgs),
    /// Show one routine's definition, effective state, and recent fires
    Show(RoutineShowArgs),
    /// Suppress a routine on this host (host-local, survives reboots)
    Pause(RoutinePauseArgs),
    /// Clear a host-local pause
    Resume(RoutineResumeArgs),
    /// Show, pause, enable, or configure the host OS sweep clock
    Clock(RoutineClockArgs),
    /// Read this host's identity and optionally install the OS clock unit
    Init(RoutineInitArgs),
}

impl RoutineSubcommand {
    fn execute_without_runtime(self, global_root: &Path) -> CommandOut {
        match self {
            Self::List(args) => args.execute_without_runtime(global_root),
            Self::Show(args) => args.execute_without_runtime(global_root),
            Self::Pause(args) => args.execute_without_runtime(global_root),
            Self::Resume(args) => args.execute_without_runtime(global_root),
            Self::Clock(args) => args.execute_without_runtime(global_root),
            Self::Init(args) => args.execute_without_runtime(global_root),
        }
    }
}
