use super::*;

use super::toggle::RoutineToggleRefusal;

/// A selected checkout is the entire discovery scope: never another store by
/// cwd, never a client-supplied path.
struct SelectedCheckout<'a>(&'a OrbitRuntime);

impl RoutineWorkspaceProvider for SelectedCheckout<'_> {
    fn discover_workspaces(&self, _: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        let runtime = self.0;
        let workspace = Workspace {
            id: runtime.workspace_id()?,
            name: runtime.workspace_label(),
            owner_machine_id: runtime
                .workspace_runtime_binding()
                .and_then(|binding| binding.owner_machine_id.clone()),
            git_remote: None,
            ship_mode: None,
            base_branch: runtime.workspace_base_branch().into(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        Ok(DiscoveredWorkspaces::single_checkout(
            workspace,
            runtime.clone(),
        ))
    }
}

/// Routine status for one selected checkout, with the same replica rule the
/// host sweep applies.
pub(crate) fn checkout_routine_statuses(
    runtime: &OrbitRuntime,
) -> Result<RoutineStatusReport, OrbitError> {
    routine_statuses_with_providers(
        &runtime.global_root(),
        RoutineMachineIdentity {
            machine_id: runtime
                .automation_machine_identity()
                .unwrap_or("local")
                .into(),
            machine_name: "Selected host".into(),
        },
        &SelectedCheckout(runtime),
        Utc::now(),
    )
}

/// Toggle one routine definition in a selected checkout [ORB-14173].
///
/// An owner checkout keeps the general coordination-write guard. A replica
/// checkout may change only the replica-local definitions it schedules for
/// itself; a definition its owner schedules is refused with the owner named,
/// and a claimed worker never toggles anything.
pub(crate) fn toggle_checkout_routine(
    runtime: &OrbitRuntime,
    name: &str,
    target: &str,
    expected_enabled: bool,
    enabled: bool,
) -> Result<RoutineToggleOutcome, OrbitError> {
    let report = checkout_edit_report(runtime)?;
    toggle_listed_routine(&report, name, None, target, expected_enabled, enabled)?
        .map_err(checkout_refusal)
}

/// The routine report a selected-checkout edit selects from, after the role
/// guard: an owner checkout keeps the general coordination-write guard, a
/// replica checkout reaches selection (which refuses owner-only routines),
/// and a claimed worker is refused.
pub(crate) fn checkout_edit_report(
    runtime: &OrbitRuntime,
) -> Result<RoutineStatusReport, OrbitError> {
    let replica =
        runtime.worker_invocation().is_none() && runtime.coordination_write_owner().is_some();
    if !replica {
        runtime.ensure_coordination_task_write_permitted()?;
    }
    checkout_routine_statuses(runtime)
}

/// A selected-checkout selection refusal as the tool surface reports it.
pub(crate) fn checkout_refusal(refusal: RoutineToggleRefusal) -> OrbitError {
    match refusal {
        RoutineToggleRefusal::OwnerAuthority { reason, .. } => {
            OrbitError::CapabilityRefused(reason)
        }
        RoutineToggleRefusal::NotFound | RoutineToggleRefusal::WorkspaceMismatch { .. } => {
            OrbitError::InvalidInput("routine unavailable in this workspace".into())
        }
    }
}
