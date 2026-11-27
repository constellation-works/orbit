//! `orbit routine` — inspect and control routines on this host [ORB-10021].
//!
//! Every subcommand operates on the global registry and host-local scheduler
//! state, so the parent command dispatches without a workspace runtime (see
//! `main.rs`), like `orbit sweep` itself. The exceptions are `enable` and
//! `disable`: they are governed operator-only operations that run through
//! the routine app's `exec`, so they open a workspace for its authorization
//! and audit, and change only a routine of that workspace so the audit row
//! names the workspace whose definition changed.

use std::ffi::OsStr;
use std::path::Path;

use clap::{Args, Subcommand};
use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_common::Env;
use orbit_common::OrbitError;
use orbit_core::Core;

use super::breaker::RoutineBreakerArgs;
use super::init::RoutineInitArgs;
use super::list::RoutineListArgs;
use super::pause::RoutinePauseArgs;
use super::resume::RoutineResumeArgs;
use super::show::RoutineShowArgs;
use super::toggle::{RoutineDisableArgs, RoutineEnableArgs};
use crate::command::CommandOut;
use crate::entry::resolved::Resolved;

#[derive(Args)]
#[command(
    about = "Inspect and control scheduled routines on this host",
    arg_required_else_help = true,
    subcommand_required = true,
    after_help = "Routine definitions are YAML files under `.orbit/routines/` in each\n\
                  registered checkout. `.orbit/` is per-user checkout state that\n\
                  git ignores, so a definition edit applies to that checkout only.\n\
                  A replica checkout schedules only its worktree GC and store GC\n\
                  routines; its other routines are listed as owner-only. Pauses are\n\
                  host-local and never synced, like circuit breakers: a routine whose\n\
                  fires fail 3 times in a row with the same error stops firing until\n\
                  a trial succeeds or `breaker reset`. `enable` and `disable` edit the definition's\n\
                  versioned `enabled` field like the dashboard toggle, are operator-only\n\
                  and are audited. A newly initialized workspace seeds `task-pilot`\n\
                  and `worktree-gc` enabled and the other shipped routines\n\
                  (`ci-failure-sweep`, `dependabot-alert-sweep`, `resident-cycle`,\n\
                  `store-gc`, `task-publication`) off until enabled. A workspace initialized earlier keeps the `enabled`\n\
                  value its definitions already hold. The host scheduler is controlled\n\
                  through `orbit clock`."
)]
pub struct RoutineCommand {
    #[command(subcommand)]
    pub command: RoutineSubcommand,
}

impl RoutineCommand {
    /// Resolve the selected global root once for every routine subcommand;
    /// none may bootstrap a workspace from the caller's cwd. `core` is the
    /// workspace the governed `enable`/`disable` verbs were opened with; they
    /// refuse a routine that belongs to another workspace.
    pub fn execute_without_runtime(
        self,
        root_override: Option<&Path>,
        workspace_selector: Option<&str>,
        core: Option<&Core>,
        env: &Env,
        resolved: &Resolved,
    ) -> CommandOut {
        let global_root = selected_global_root(root_override, env, resolved)?;
        self.command
            .execute_without_runtime(&global_root, workspace_selector, core)
    }
}

#[derive(Subcommand)]
pub enum RoutineSubcommand {
    /// List routines with toggles, next-due, and last fire, optionally for one workspace
    List(RoutineListArgs),
    /// Show one routine's definition, effective state, and recent fires
    Show(RoutineShowArgs),
    /// Enable a routine's versioned definition (operator only)
    Enable(RoutineEnableArgs),
    /// Disable a routine's versioned definition (operator only)
    Disable(RoutineDisableArgs),
    /// Suppress a routine on this host (host-local, survives reboots)
    Pause(RoutinePauseArgs),
    /// Clear a host-local pause
    Resume(RoutineResumeArgs),
    /// Reset or bypass a routine's circuit breaker on this host
    Breaker(RoutineBreakerArgs),
    /// Read this host's identity and optionally install the OS clock unit
    Init(RoutineInitArgs),
}

impl RoutineSubcommand {
    fn execute_without_runtime(
        self,
        global_root: &Path,
        workspace_selector: Option<&str>,
        core: Option<&Core>,
    ) -> CommandOut {
        match self {
            Self::List(args) => args.execute_without_runtime(global_root, workspace_selector),
            Self::Show(args) => args.execute_without_runtime(global_root),
            Self::Enable(args) => args.execute_without_runtime(global_root, core),
            Self::Disable(args) => args.execute_without_runtime(global_root, core),
            Self::Pause(args) => args.execute_without_runtime(global_root),
            Self::Resume(args) => args.execute_without_runtime(global_root),
            Self::Breaker(args) => args.execute_without_runtime(global_root),
            Self::Init(args) => args.execute_without_runtime(global_root),
        }
    }
}

fn selected_global_root(
    root_override: Option<&Path>,
    env: &Env,
    resolved: &Resolved,
) -> Result<std::path::PathBuf, OrbitError> {
    let has_env_override = env
        .orbit_root()
        .and_then(OsStr::to_str)
        .is_some_and(|root| !root.trim().is_empty());
    if root_override.is_some() || has_env_override {
        let cwd = std::env::current_dir().map_err(|error| OrbitError::Io(error.to_string()))?;
        return RegisteredRuntimeFactory::resolve_roots_for_cwd(&cwd, root_override)
            .map(|roots| roots.global_root);
    }
    resolved.global_orbit_dir().map(Path::to_path_buf)
}
