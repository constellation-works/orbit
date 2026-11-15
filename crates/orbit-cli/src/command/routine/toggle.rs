use std::path::Path;

use crate::command::{CommandOut, Payload};
use clap::Args;
use orbit_cmd::registry_routines::routine_statuses;
use orbit_core::application::routines::{RoutineEditRefusal, toggle_listed_routine};
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
    // Observe the definition's current source, state and target, then hand them
    // back as the expectation the atomic edit re-checks against the file it
    // rewrites. An unlisted name observes nothing; the shared selection refuses it.
    let (source_workspace, target, expected_enabled) = report
        .statuses
        .iter()
        .find(|status| status.routine.definition.name == name)
        .map(|status| {
            let routine = &status.routine;
            (
                routine.source_workspace.clone(),
                routine.definition.target.as_ref_string(),
                routine.definition.enabled,
            )
        })
        .unwrap_or_default();
    let changed = toggle_listed_routine(
        &report,
        name,
        Some((&source_workspace, &shared_root)),
        &target,
        expected_enabled,
        enabled,
    )?
    .map_err(|refusal| match refusal {
        RoutineEditRefusal::WorkspaceMismatch {
            ref source_workspace,
            ..
        } => OrbitError::InvalidInput(format!(
            "{}: routine '{name}' belongs to workspace '{source_workspace}', not the selected \
             workspace; rerun with `--workspace {source_workspace}`",
            refusal.code()
        )),
        other => other.into(),
    })?
    .changed;
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
