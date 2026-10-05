//! Registry-neutral seam for reads that span more than one workspace.
//!
//! Core owns no workspace catalog: `orbit-registry` sits above it in the crate
//! graph. A federated read therefore needs a higher composition layer to answer
//! two questions — *which checkouts does this scope cover* and *open a runtime
//! for one of them* — while Core keeps ownership of fan-out, fusion,
//! attribution, and degradation notes [ORB-11027].
//!
//! The seam mirrors [`OrbitRuntime::with_coordination_write_owner`]: a
//! standalone runtime constructed without a catalog still works, and simply
//! refuses any scope wider than its own workspace.

use std::path::PathBuf;

use orbit_common::OrbitError;

use crate::runtime::OrbitRuntime;

/// Which workspaces one search covers.
///
/// [`WorkspaceScope::Current`] is the default and is not merely "a scope of
/// one": it takes the untouched single-workspace path, so an existing caller
/// observes byte-identical results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum WorkspaceScope {
    #[default]
    Current,
    /// Explicit selectors — registered name, logical `ws_*` ID, or an absolute
    /// local checkout path. Resolution belongs to the catalog implementation.
    Selectors(Vec<String>),
    /// Every active checkout registered on this machine.
    AllRegistered,
}

impl WorkspaceScope {
    /// Whether this scope needs a catalog to answer.
    pub fn is_federated(&self) -> bool {
        !matches!(self, Self::Current)
    }

    /// Build a scope from the two independent inputs every surface collects:
    /// a list of selectors and an "everything registered" switch.
    ///
    /// Blank selectors are dropped so an empty list is indistinguishable from
    /// no list at all, which keeps `--workspace ""` from silently federating.
    pub fn from_inputs(selectors: Vec<String>, all_registered: bool) -> Self {
        let selectors = selectors
            .into_iter()
            .map(|selector| selector.trim().to_string())
            .filter(|selector| !selector.is_empty())
            .collect::<Vec<_>>();
        match (selectors.is_empty(), all_registered) {
            (_, true) => Self::AllRegistered,
            (true, false) => Self::Current,
            (false, false) => Self::Selectors(selectors),
        }
    }
}

/// One local checkout a federated read may open.
///
/// Deliberately plain data: no `Workspace`, `WorkspaceCheckout`, or registry
/// handle crosses into Core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FederatedWorkspaceTarget {
    /// Logical `ws_*` catalog ID.
    pub workspace_id: String,
    /// Registered workspace name, as `orbit workspace list` shows it.
    pub name: String,
    pub repo_root: PathBuf,
}

/// Resolves a [`WorkspaceScope`] and opens runtimes for what it covers.
///
/// Two methods rather than one `Vec<(target, runtime)>` so that a single
/// unopenable checkout — stale path, another machine's replica, a removed
/// worktree — degrades to a note on that workspace instead of failing the
/// whole query.
pub trait WorkspaceCatalog: Send + Sync {
    /// Checkouts covered by `scope`, in a stable order.
    fn resolve_scope(
        &self,
        scope: &WorkspaceScope,
    ) -> Result<Vec<FederatedWorkspaceTarget>, OrbitError>;

    /// Open a runtime bound to one resolved target.
    fn open(&self, target: &FederatedWorkspaceTarget) -> Result<OrbitRuntime, OrbitError>;
}
