//! Registry-backed resolution for Core's federated-search scope.
//!
//! Core owns the fan-out, fusion, and attribution; it deliberately owns no
//! workspace catalog, because `orbit-registry` sits above it in the crate
//! graph. This module is the composition point that closes that gap: it turns
//! a [`WorkspaceScope`] into registered checkouts and opens a runtime for one
//! [ORB-11027].
//!
//! It adds no dependency edge — `orbit-cmd` already joins Core to Registry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use orbit_common::OrbitError;
use orbit_core::{FederatedWorkspaceTarget, OrbitRuntime, WorkspaceCatalog, WorkspaceScope};
use orbit_registry::workspace_registry;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus};

use crate::registry_runtime::RegisteredRuntimeFactory;

/// Resolves federated scope against this machine's workspace registry.
///
/// One federated query reads `workspaces.json` exactly once: [`resolve_scope`]
/// keeps the records it resolved, and the fan-out's `open` calls — one per
/// workspace, running concurrently — read from that snapshot instead of
/// re-parsing the file and re-validating every checkout N more times
/// [DANI-10365].
///
/// The snapshot is *replaced* by the next `resolve_scope`, not accumulated, so
/// it is a per-query view rather than a cache that can go stale behind a
/// long-lived runtime. A target the snapshot does not cover — one resolved by
/// a different catalog, or overwritten by a concurrent query — falls back to a
/// registry lookup, so no caller loses the "no longer registered" answer.
///
/// [`resolve_scope`]: WorkspaceCatalog::resolve_scope
#[derive(Debug, Clone)]
pub struct RegistryWorkspaceCatalog {
    global_root: PathBuf,
    resolved: Arc<Mutex<BTreeMap<String, RegisteredCheckout>>>,
}

/// The registry records needed to open one checkout, resolved once.
#[derive(Debug, Clone)]
pub(crate) struct RegisteredCheckout {
    workspace: Workspace,
    checkout: WorkspaceCheckout,
}

impl RegisteredCheckout {
    fn of(workspace: &Workspace, checkout: &WorkspaceCheckout) -> Self {
        Self {
            workspace: workspace.clone(),
            checkout: checkout.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn repo_root(&self) -> &Path {
        &self.checkout.repo_root
    }
}

impl RegistryWorkspaceCatalog {
    pub fn new(global_root: impl Into<PathBuf>) -> Self {
        Self {
            global_root: global_root.into(),
            resolved: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    fn load_registry(&self) -> Result<WorkspaceRegistry, OrbitError> {
        workspace_registry::load_registry_from(&workspace_registry::registry_path_for(
            &self.global_root,
        ))
    }

    /// Replace the snapshot with what this scope resolution covers.
    fn remember(&self, resolved: &[RegisteredCheckout]) {
        let mut snapshot = self.resolved.lock().unwrap_or_else(PoisonError::into_inner);
        *snapshot = resolved
            .iter()
            .map(|entry| (entry.workspace.id.clone(), entry.clone()))
            .collect();
    }

    /// The records `open` will use for `target`: the snapshot when it covers
    /// the workspace, a fresh registry lookup otherwise.
    pub(crate) fn resolve_target(
        &self,
        target: &FederatedWorkspaceTarget,
    ) -> Result<RegisteredCheckout, OrbitError> {
        let remembered = self
            .resolved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&target.workspace_id)
            .cloned();
        match remembered {
            Some(resolved) => Ok(resolved),
            None => self.lookup_target(target),
        }
    }

    fn lookup_target(
        &self,
        target: &FederatedWorkspaceTarget,
    ) -> Result<RegisteredCheckout, OrbitError> {
        let registry = self.load_registry()?;
        registry
            .workspaces
            .iter()
            .find(|workspace| workspace.id == target.workspace_id)
            .zip(
                registry
                    .checkouts
                    .iter()
                    .find(|checkout| checkout.workspace_id == target.workspace_id),
            )
            .map(|(workspace, checkout)| RegisteredCheckout::of(workspace, checkout))
            .ok_or_else(|| {
                OrbitError::WorkspaceError(format!(
                    "workspace '{}' is no longer registered on this machine",
                    target.name
                ))
            })
    }
}

impl WorkspaceCatalog for RegistryWorkspaceCatalog {
    fn resolve_scope(
        &self,
        scope: &WorkspaceScope,
    ) -> Result<Vec<FederatedWorkspaceTarget>, OrbitError> {
        let selected = match scope {
            // Core never asks a catalog to resolve its own checkout.
            WorkspaceScope::Current => Vec::new(),
            WorkspaceScope::AllRegistered => {
                let registry = self.load_registry()?;
                workspace_registry::local_workspaces(&registry)
                    .filter(|(workspace, _)| workspace.status == WorkspaceStatus::Active)
                    .map(|(workspace, checkout)| RegisteredCheckout::of(workspace, checkout))
                    .collect()
            }
            // A selector is resolved through the same fail-closed grammar the
            // `--workspace` binder uses, so an unknown or ambiguous name is
            // named rather than quietly dropped from the scope. Every selector
            // reads the one registry load, not its own.
            WorkspaceScope::Selectors(selectors) => {
                let registry =
                    RegisteredRuntimeFactory::load_registry_for_selectors(&self.global_root)?;
                let identity = orbit_registry::inspect_machine_identity(&self.global_root)?;
                selectors
                    .iter()
                    .map(|selector| {
                        RegisteredRuntimeFactory::resolve_selector_in(
                            &registry,
                            selector,
                            identity.id(),
                        )
                        .map(|selected| RegisteredCheckout {
                            workspace: selected.workspace,
                            checkout: selected.checkout,
                        })
                    })
                    .collect::<Result<Vec<_>, OrbitError>>()?
            }
        };

        let selected = dedupe_by_workspace_id(selected);
        let targets = selected
            .iter()
            .map(|entry| target_for(&entry.workspace, &entry.checkout))
            .collect();
        self.remember(&selected);
        Ok(targets)
    }

    fn open(&self, target: &FederatedWorkspaceTarget) -> Result<OrbitRuntime, OrbitError> {
        let resolved = self.resolve_target(target)?;
        RegisteredRuntimeFactory::open_registered_checkout(
            &self.global_root,
            &resolved.workspace,
            &resolved.checkout,
        )
    }
}

fn target_for(workspace: &Workspace, checkout: &WorkspaceCheckout) -> FederatedWorkspaceTarget {
    FederatedWorkspaceTarget {
        workspace_id: workspace.id.clone(),
        name: workspace.name.clone(),
        repo_root: checkout.repo_root.clone(),
    }
}

/// Two selectors can name the same workspace (`ws_*`, name, and path all
/// resolve to one checkout). Opening it twice would double-count its hits in
/// the fused list, so the first mention wins and order is preserved.
fn dedupe_by_workspace_id(selected: Vec<RegisteredCheckout>) -> Vec<RegisteredCheckout> {
    let mut seen = std::collections::BTreeSet::new();
    selected
        .into_iter()
        .filter(|entry| seen.insert(entry.workspace.id.clone()))
        .collect()
}

/// Attach registry-backed federated search to a runtime this crate opened.
pub(crate) fn attach(runtime: OrbitRuntime, global_root: &Path) -> OrbitRuntime {
    runtime.with_workspace_catalog(std::sync::Arc::new(RegistryWorkspaceCatalog::new(
        global_root,
    )))
}
