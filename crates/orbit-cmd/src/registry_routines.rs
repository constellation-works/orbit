//! Registry composition over Core's scheduler kernels: which checkouts this
//! host evaluates schedules for, and who this host is.
//!
//! Both come exclusively from `host.toml` and `workspaces.json`.

use std::path::Path;

use chrono::Utc;
use orbit_automation::routines::{
    DiscoveredWorkspaces, RoutineHostIdentity, RoutineLoadError, RoutineStatusReport,
    RoutineWorkspaceProvider, SweepOptions, SweepOutcome,
};
use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_types::workspace::{WorkspaceCheckoutRole, WorkspaceStatus};

use orbit_registry::host_identity::{HostIdentity, load_host_identity};
use orbit_registry::workspace_registry;

use crate::registry_runtime::RegisteredRuntimeFactory;

struct RegistryRoutineEnvironment {
    identity: HostIdentity,
    /// Registered workspace this pass is restricted to, resolved from the
    /// caller's `--workspace` selector. `None` visits every local workspace.
    workspace_filter: Option<String>,
}

impl RegistryRoutineEnvironment {
    /// An unknown, unregistered, or inactive selector fails closed here,
    /// before the sweep touches any scheduler state: an operator asking for
    /// one workspace must never silently get the whole host.
    fn load(global_root: &Path, workspace_selector: Option<&str>) -> Result<Self, OrbitError> {
        let workspace_filter = workspace_selector
            .map(|selector| {
                RegisteredRuntimeFactory::resolve_workspace_selector(global_root, selector)
                    .map(|selected| selected.workspace.id)
            })
            .transpose()?;
        Ok(Self {
            identity: load_host_identity(global_root)?,
            workspace_filter,
        })
    }

    fn local_host(&self) -> RoutineHostIdentity {
        RoutineHostIdentity {
            machine_id: self.identity.machine_id.clone(),
            host_id: self.identity.host_id.clone(),
        }
    }
}

impl RoutineWorkspaceProvider for RegistryRoutineEnvironment {
    type Host = OrbitRuntime;

    fn discover_workspaces(
        &self,
        global_root: &Path,
    ) -> Result<DiscoveredWorkspaces<OrbitRuntime>, OrbitError> {
        discover_registered_workspaces(global_root, self.workspace_filter.as_deref())
    }
}

/// Discover the checkouts this host evaluates schedules for, optionally
/// restricted to one registered workspace id: every active **owner** checkout
/// with a `.orbit/` directory. Registration is the whole opt-in [ORB-12236];
/// a replica is skipped because it cannot write the owner's coordination
/// store. The provider delegates here so this production path can be
/// exercised with an explicit global root.
pub(crate) fn discover_registered_workspaces(
    global_root: &Path,
    workspace_filter: Option<&str>,
) -> Result<DiscoveredWorkspaces<OrbitRuntime>, OrbitError> {
    let registry_path = workspace_registry::registry_path_for(global_root);
    let registry = workspace_registry::with_registry_lock(&registry_path, || {
        let mut registry = workspace_registry::load_registry_from(&registry_path)?;
        workspace_registry::validate_workspaces(&mut registry);
        workspace_registry::save_registry_to(&registry, &registry_path)?;
        Ok(registry)
    })?;

    let mut discovered = DiscoveredWorkspaces::default();
    for (workspace, checkout) in workspace_registry::local_workspaces(&registry) {
        if workspace.status != WorkspaceStatus::Active || !checkout.orbit_dir.exists() {
            continue;
        }
        if checkout.role == Some(WorkspaceCheckoutRole::Replica) {
            continue;
        }
        if workspace_filter.is_some_and(|selected| selected != workspace.id) {
            continue;
        }
        match RegisteredRuntimeFactory::open_registered_checkout(global_root, workspace, checkout) {
            Ok(runtime) => discovered.entries.push((workspace.clone(), runtime)),
            Err(error) => discovered.errors.push(RoutineLoadError {
                source_workspace: workspace.name.clone(),
                path: Some(checkout.orbit_dir.clone()),
                message: format!("failed to open workspace runtime: {error}"),
            }),
        }
    }
    Ok(discovered)
}

pub fn routine_statuses(global_root: &Path) -> Result<RoutineStatusReport, OrbitError> {
    let environment = RegistryRoutineEnvironment::load(global_root, None)?;
    orbit_automation::routines::routine_statuses_with_providers(
        global_root,
        environment.local_host(),
        &environment,
        Utc::now(),
    )
}

/// Run one sweep pass over this host's registered workspaces, or only the one
/// named by `workspace_selector`.
pub fn run_sweep(
    options: SweepOptions,
    workspace_selector: Option<&str>,
) -> Result<SweepOutcome, OrbitError> {
    let global_root = workspace_registry::global_orbit_dir()?;
    let environment = RegistryRoutineEnvironment::load(&global_root, workspace_selector)?;
    // The pass itself runs against the root Core resolves, which inside a
    // managed run may be an explicit registry root rather than `~/.orbit`.
    orbit_automation::routines::run_sweep_with_providers(
        &orbit_core::runtime::resolve_global_root()?,
        options,
        environment.local_host(),
        &environment,
    )
}

/// As [`run_sweep`], against an explicit global root.
pub fn run_sweep_at(
    global_root: &Path,
    options: SweepOptions,
    workspace_selector: Option<&str>,
) -> Result<SweepOutcome, OrbitError> {
    let environment = RegistryRoutineEnvironment::load(global_root, workspace_selector)?;
    orbit_automation::routines::run_sweep_at_with_providers(
        global_root,
        options,
        environment.local_host(),
        &environment,
    )
}
