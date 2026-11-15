//! The `workspace-convergence` row: registered workspaces on this host whose
//! managed artifacts lag the defaults this binary ships.
//!
//! `orbit workspace sync --all` is the fix; this row is its read-only
//! `--check` form, so a workspace nobody synced after an upgrade is named
//! instead of staying behind unnoticed.

use std::collections::BTreeSet;

use orbit_core::{OrbitRuntime, RoutineSeedIdentity, reconcile_workspace_managed_artifacts};
use orbit_registry::workspace_registry;
use orbit_registry::{MachineIdentityState, inspect_machine_identity};

use super::{WorkspaceDoctorResult, WorkspaceDoctorStatus};

const CHECK: &str = "workspace-convergence";
const SYNC_ALL: &str =
    "Run `orbit workspace sync --all` on this host (preview with `--check --all`).";

pub(super) fn host_workspace_convergence_row(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let global_root = runtime.global_root();
    let registry = match workspace_registry::load_registry_from(
        &workspace_registry::registry_path_for(&global_root),
    ) {
        Ok(registry) => registry,
        Err(error) => {
            return row(
                WorkspaceDoctorStatus::Warning,
                format!("cannot read the workspace registry: {error}"),
                Some("Repair the registry, then rerun `orbit doctor`.".to_string()),
            );
        }
    };
    let machine_id = match inspect_machine_identity(&global_root) {
        Ok(MachineIdentityState::Present(identity)) => identity.id,
        Ok(MachineIdentityState::Absent) => {
            return row(
                WorkspaceDoctorStatus::Skipped,
                "this host has no initialized identity, so workspaces cannot be converged yet"
                    .to_string(),
                Some("Run `orbit init`.".to_string()),
            );
        }
        Err(error) => {
            return row(
                WorkspaceDoctorStatus::Warning,
                format!("cannot read this host's identity: {error}"),
                Some("Repair the machine identity, then rerun `orbit doctor`.".to_string()),
            );
        }
    };

    let mut inspected = 0;
    let mut pending = Vec::new();
    let mut failures = Vec::new();
    let mut seen_roots = BTreeSet::new();
    for checkout in &registry.checkouts {
        // A checkout whose data directory is gone has nothing to converge;
        // `orbit workspace sync --all` reports it as skipped.
        if !checkout.orbit_dir.is_dir() || !seen_roots.insert(&checkout.orbit_dir) {
            continue;
        }
        let Some(workspace) =
            workspace_registry::find_workspace_by_id(&registry, &checkout.workspace_id)
        else {
            continue;
        };
        inspected += 1;
        let report = RoutineSeedIdentity::new(&workspace.name, &machine_id, &workspace.base_branch)
            .and_then(|identity| {
                reconcile_workspace_managed_artifacts(
                    &global_root,
                    &checkout.orbit_dir,
                    Some(&identity),
                    &workspace.base_branch,
                    true,
                )
            });
        match report {
            Ok(report) if report.has_pending_changes() => pending.push(workspace.name.clone()),
            Ok(_) => {}
            Err(error) => failures.push(format!("{}: {error}", workspace.name)),
        }
    }

    if inspected == 0 {
        return row(
            WorkspaceDoctorStatus::Skipped,
            "no registered workspace on this host has a data directory to converge".to_string(),
            None,
        );
    }
    if pending.is_empty() && failures.is_empty() {
        return row(
            WorkspaceDoctorStatus::Ok,
            format!(
                "all {inspected} registered workspace(s) on this host match the defaults this binary ships"
            ),
            None,
        );
    }
    let mut message = String::new();
    if !pending.is_empty() {
        message.push_str(&format!(
            "{} of {inspected} registered workspace(s) have pending managed-artifact changes: {}",
            pending.len(),
            pending.join(", ")
        ));
    }
    if !failures.is_empty() {
        if !message.is_empty() {
            message.push_str("; ");
        }
        message.push_str(&format!("cannot inspect {}", failures.join("; ")));
    }
    row(
        WorkspaceDoctorStatus::Warning,
        message,
        Some(SYNC_ALL.to_string()),
    )
}

fn row(
    status: WorkspaceDoctorStatus,
    message: String,
    remediation: Option<String>,
) -> WorkspaceDoctorResult {
    WorkspaceDoctorResult {
        duration_ms: 0,
        check_name: CHECK.into(),
        status,
        message,
        remediation,
    }
}
