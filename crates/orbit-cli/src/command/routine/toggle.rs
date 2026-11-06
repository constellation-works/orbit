use std::path::Path;

use crate::command::{CommandOut, Payload};
use clap::Args;
use orbit_cmd::registry_routines::routine_statuses;
use orbit_core::application::routines::{
    RoutineToggleOutcome, RoutineToggleRefusal, toggle_listed_routine,
};
use orbit_core::{OrbitError, OrbitRuntime};
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
    pub fn execute_without_runtime(
        self,
        global_root: &Path,
        runtime: Option<&OrbitRuntime>,
    ) -> CommandOut {
        set_enabled(global_root, runtime, &self.name, true)
    }
}

impl RoutineDisableArgs {
    pub fn execute_without_runtime(
        self,
        global_root: &Path,
        runtime: Option<&OrbitRuntime>,
    ) -> CommandOut {
        set_enabled(global_root, runtime, &self.name, false)
    }
}

/// Flip the routine's versioned `enabled` field through the selection and
/// atomic edit the dashboard toggle uses. The caller already passed the
/// operator-only `routine enable`/`routine disable` authorization at the CLI
/// chokepoint (which needs a runtime, so these two verbs open one) and the
/// command audit row is written around the call.
///
/// Routine names are selected host-wide, but the audit row is recorded under
/// the workspace the runtime is bound to (the `--workspace` selection, else the
/// cwd's). A routine from any other workspace is refused before any write, so
/// the file that changes and the workspace the audit row names cannot differ.
fn set_enabled(
    global_root: &Path,
    runtime: Option<&OrbitRuntime>,
    name: &str,
    enabled: bool,
) -> CommandOut {
    let runtime = runtime.ok_or_else(|| {
        OrbitError::Execution(
            "routine enable/disable requires a workspace runtime for authorization and audit"
                .to_string(),
        )
    })?;
    let shared_root = runtime.shared_root();
    let report = routine_statuses(global_root)?;
    // Observe the definition's current state and target, then hand both back as
    // the expectation the atomic edit re-checks against the file it rewrites.
    let Some(status) = report
        .statuses
        .iter()
        .find(|status| status.routine.definition.name == name)
    else {
        return match report
            .owner_only
            .iter()
            .find(|owned| owned.routine.definition.name == name)
        {
            Some(owned) => Err(OrbitError::CapabilityRefused(owned.reason.clone())),
            None => Err(OrbitError::InvalidInput(format!(
                "no routine named '{name}' (see `orbit routine list`)"
            ))),
        };
    };
    let routine = &status.routine;
    if routine.source_orbit_dir != shared_root {
        return Err(OrbitError::InvalidInput(format!(
            "routine '{name}' belongs to workspace '{}', not the selected workspace; \
             rerun with `--workspace {}`",
            routine.source_workspace, routine.source_workspace
        )));
    }
    let target = status.routine.definition.target.as_ref_string();
    let expected_enabled = status.routine.definition.enabled;
    let outcome = toggle_listed_routine(
        &report,
        name,
        Some((&routine.source_workspace, &shared_root)),
        &target,
        expected_enabled,
        enabled,
    )?
    .map_err(|refusal| match refusal {
        RoutineToggleRefusal::OwnerAuthority { reason, .. } => {
            OrbitError::CapabilityRefused(reason)
        }
        RoutineToggleRefusal::NotFound | RoutineToggleRefusal::WorkspaceMismatch { .. } => {
            OrbitError::InvalidInput(format!(
                "no routine named '{name}' (see `orbit routine list`)"
            ))
        }
    })?;
    let changed = match outcome {
        RoutineToggleOutcome::Changed => true,
        RoutineToggleOutcome::Unchanged => false,
        RoutineToggleOutcome::Conflict { .. } | RoutineToggleOutcome::TargetConflict { .. } => {
            return Err(OrbitError::Execution(format!(
                "routine '{name}' changed while this command ran; re-check `orbit routine show {name}` and retry"
            )));
        }
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
