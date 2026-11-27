use std::path::Path;

use crate::command::{CommandOut, Payload};
use clap::Args;
use orbit_cmd::registry_routines::routine_statuses;
use orbit_common::OrbitError;
use orbit_core::Core;
use orbit_core::app::routine::{self, Command, Outcome, RoutineEditOutcome};
use serde_json::json;

#[derive(Args)]
pub struct RoutineEnableArgs {
    /// Routine name, as shown by `orbit routine list`.
    pub name: String,
}

#[derive(Args)]
pub struct RoutineDisableArgs {
    /// Routine name, as shown by `orbit routine list`.
    pub name: String,
}

impl RoutineEnableArgs {
    pub fn execute_without_runtime(self, global_root: &Path, core: Option<&Core>) -> CommandOut {
        set_enabled(global_root, core, self.name, true)
    }
}

impl RoutineDisableArgs {
    pub fn execute_without_runtime(self, global_root: &Path, core: Option<&Core>) -> CommandOut {
        set_enabled(global_root, core, self.name, false)
    }
}

/// Flip the routine's versioned `enabled` field through the routine app,
/// which holds the caller to the operator-only `routine enable`/`routine
/// disable` authorization, writes the command audit row and runs the
/// selection and atomic edit the dashboard toggle uses. The app reads the
/// host's routine status only once the caller is admitted.
fn set_enabled(global_root: &Path, core: Option<&Core>, name: String, enabled: bool) -> CommandOut {
    let core = core.ok_or_else(|| {
        OrbitError::Execution(
            "routine enable/disable requires a workspace runtime for authorization and audit"
                .to_string(),
        )
    })?;
    let statuses = {
        let global_root = global_root.to_path_buf();
        Box::new(move || routine_statuses(&global_root))
    };
    let outcome = routine::exec(
        core,
        Command::SetEnabled {
            name: name.clone(),
            enabled,
            statuses,
        },
    )?;
    let Outcome::EnabledSet(RoutineEditOutcome { changed }) = outcome else {
        return Err(OrbitError::Execution(
            "routine enable/disable answered with another command's outcome".to_string(),
        ));
    };
    let state = if enabled { "enabled" } else { "disabled" };
    let text = if changed {
        format!("{state} '{name}'")
    } else {
        format!("'{name}' is already {state}")
    };
    Ok(Payload::detail(
        json!({ "routine": name, "enabled": enabled, "changed": changed }),
        text,
    )
    .into())
}
