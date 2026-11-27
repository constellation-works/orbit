//! `orbit routine enable|disable`: flip a routine's versioned `enabled`
//! field through the selection and atomic edit the dashboard toggle uses.

use orbit_common::OrbitError;

use super::Statuses;
use super::status::toggle_listed_routine;
use super::{RoutineEditOutcome, RoutineEditRefusal};
use crate::OrbitRuntime;

/// Routine names are selected host-wide, but the audit row is recorded under
/// the workspace the runtime is bound to (the `--workspace` selection, else
/// the cwd's). A routine from any other workspace is refused before any
/// write, so the file that changes and the workspace the audit row names
/// cannot differ.
pub(super) fn set_enabled(
    runtime: &OrbitRuntime,
    name: &str,
    enabled: bool,
    statuses: Statuses,
) -> Result<RoutineEditOutcome, OrbitError> {
    let shared_root = runtime.shared_root();
    let report = statuses()?;
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
    toggle_listed_routine(
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
    })
}
