//! Workspace and checkout lookup by selector, exact ID, and local path.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry};

/// Finds a workspace by id or name, rejecting ambiguous selectors.
pub fn find_workspace<'a>(
    registry: &'a WorkspaceRegistry,
    id_or_name: &str,
) -> Result<Option<&'a Workspace>, OrbitError> {
    match resolve_logical_workspace_match(registry, id_or_name) {
        LogicalWorkspaceMatch::Found(workspace) => Ok(Some(workspace)),
        LogicalWorkspaceMatch::NotFound => Ok(None),
        LogicalWorkspaceMatch::Ambiguous => Err(ambiguous_workspace_selector(id_or_name)),
    }
}

/// Finds a workspace by its exact catalog ID.
///
/// Checkout bindings and other persisted relations already store
/// `workspace_id`. Those lookups must not reuse [`find_workspace`], which
/// treats the argument as an id-or-name selector and fails closed when one
/// workspace's ID equals another's name.
pub fn find_workspace_by_id<'a>(
    registry: &'a WorkspaceRegistry,
    workspace_id: &str,
) -> Option<&'a Workspace> {
    registry
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
}

/// Resolve a logical selector (registered name or `ws_*` id) to exactly one workspace.
///
/// The same fail-closed name/id grammar is used by the CLI `--workspace` flag
/// and MCP `resolve_workspace`. First-match is not
/// enough — two workspaces sharing a name must not silently pick one.
pub fn resolve_logical_workspace<'a>(
    registry: &'a WorkspaceRegistry,
    selector: &str,
) -> Result<&'a Workspace, OrbitError> {
    match resolve_logical_workspace_match(registry, selector) {
        LogicalWorkspaceMatch::Found(workspace) => Ok(workspace),
        LogicalWorkspaceMatch::NotFound => Err(unknown_workspace_selector(selector)),
        LogicalWorkspaceMatch::Ambiguous => Err(ambiguous_workspace_selector(selector)),
    }
}

enum LogicalWorkspaceMatch<'a> {
    Found(&'a Workspace),
    NotFound,
    Ambiguous,
}

fn resolve_logical_workspace_match<'a>(
    registry: &'a WorkspaceRegistry,
    selector: &str,
) -> LogicalWorkspaceMatch<'a> {
    let mut matches = registry
        .workspaces
        .iter()
        .filter(|workspace| workspace.id == selector || workspace.name == selector);
    let Some(workspace) = matches.next() else {
        return LogicalWorkspaceMatch::NotFound;
    };
    if matches.next().is_some() {
        LogicalWorkspaceMatch::Ambiguous
    } else {
        LogicalWorkspaceMatch::Found(workspace)
    }
}

pub(crate) fn unknown_workspace_selector(selector: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "unknown workspace selector '{selector}'; pass a registered workspace name, a logical workspace ID, or an absolute local checkout path"
    ))
}

pub(crate) fn ambiguous_workspace_selector(selector: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "ambiguous workspace selector '{selector}'; it matches more than one registered workspace"
    ))
}

/// Finds the local checkout for a workspace ID or name.
pub fn find_checkout<'a>(
    registry: &'a WorkspaceRegistry,
    id_or_name: &str,
) -> Result<Option<&'a WorkspaceCheckout>, OrbitError> {
    let Some(workspace) = find_workspace(registry, id_or_name)? else {
        return Ok(None);
    };
    Ok(find_checkout_by_id(registry, &workspace.id))
}

/// Finds the local checkout for an exact workspace ID.
///
/// Persisted relations and resolved bindings already store `workspace_id`.
/// Those lookups must not reuse [`find_checkout`], which treats the argument
/// as an id-or-name selector and fails closed when one workspace's ID equals
/// another's name.
pub fn find_checkout_by_id<'a>(
    registry: &'a WorkspaceRegistry,
    workspace_id: &str,
) -> Option<&'a WorkspaceCheckout> {
    registry
        .checkouts
        .iter()
        .find(|checkout| checkout.workspace_id == workspace_id)
}

/// Iterates logical workspaces that have a machine-local checkout binding.
pub fn local_workspaces(
    registry: &WorkspaceRegistry,
) -> impl Iterator<Item = (&Workspace, &WorkspaceCheckout)> {
    registry.checkouts.iter().filter_map(|checkout| {
        find_workspace_by_id(registry, &checkout.workspace_id)
            .map(|workspace| (workspace, checkout))
    })
}

/// Finds the local checkout for a path using longest-prefix matching across
/// its repository root and explicit path overrides.
///
/// The path matches as given and as the filesystem resolves it, with a tail
/// that no longer exists kept. A checkout is recorded at its resolved path, so
/// a spelling through a symlinked ancestor (macOS `/tmp` and `/var`, a
/// symlinked `~/workspace`) must still name a checkout whose directory has
/// since been deleted, where it can no longer be canonicalized whole.
pub fn find_checkout_by_path<'a>(
    registry: &'a WorkspaceRegistry,
    cwd: &Path,
) -> Option<&'a WorkspaceCheckout> {
    let resolved = resolve_with_missing_tail(cwd);
    let mut best_match: Option<(&WorkspaceCheckout, usize)> = None;

    for checkout in &registry.checkouts {
        for candidate in std::iter::once(&checkout.repo_root).chain(&checkout.path_overrides) {
            if !cwd.starts_with(candidate) && !resolved.starts_with(candidate) {
                continue;
            }
            let candidate_len = candidate.as_os_str().len();
            if best_match.is_none_or(|(_, current_len)| candidate_len > current_len) {
                best_match = Some((checkout, candidate_len));
            }
        }
    }

    best_match.map(|(checkout, _)| checkout)
}

/// `path` with its deepest existing ancestor resolved by the filesystem and
/// the names below it kept as spelled. A path with no resolvable ancestor is
/// returned unchanged.
fn resolve_with_missing_tail(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        if let Ok(resolved) = current.canonicalize() {
            return missing
                .iter()
                .rev()
                .fold(resolved, |resolved, name| resolved.join(name));
        }
        match (current.file_name(), current.parent()) {
            (Some(name), Some(parent)) => {
                missing.push(name);
                current = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Finds the logical workspace for a path, but only through a machine-local
/// checkout binding. Checkoutless catalog entries can never match a path.
pub fn find_workspace_by_path<'a>(
    registry: &'a WorkspaceRegistry,
    cwd: &Path,
) -> Option<&'a Workspace> {
    let checkout = find_checkout_by_path(registry, cwd)?;
    find_workspace_by_id(registry, &checkout.workspace_id)
}
