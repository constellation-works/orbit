use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use orbit_cmd::registry_runtime::{RegisteredRuntimeFactory, global_root_for};
use orbit_core::OrbitError;
use orbit_registry::workspace_registry;
use orbit_types::workspace::{WorkspaceCheckout, WorkspaceRegistry};

/// Where an `orbit mcp init` / `orbit mcp remove` run writes, and what the
/// generated server entry binds to.
#[derive(Debug, Clone)]
pub(super) struct WorkspaceLayout {
    pub(super) repo_root: PathBuf,
    pub(super) orbit_root: PathBuf,
    /// Logical workspace ID (`ws_*`) this checkout is registered under, or
    /// `None` when no registry on this machine knows it.
    pub(super) workspace_id: Option<String>,
}

pub(super) fn resolve_workspace_layout(
    root_override: Option<&Path>,
) -> Result<WorkspaceLayout, OrbitError> {
    let cwd = env::current_dir().map_err(|err| OrbitError::Io(err.to_string()))?;
    resolve_workspace_layout_for_cwd(&cwd, root_override)
}

/// Resolve the checkout these MCP setup commands operate on.
///
/// An explicit `--root` or `ORBIT_ROOT` answers first, matching the precedence
/// used by the other root-aware surfaces. When no root is selected, the
/// workspace registry identifies relocated checkouts before filesystem
/// walk-up, because it is the only source that knows a checkout whose Orbit
/// root lives outside the repository.
pub(super) fn resolve_workspace_layout_for_cwd(
    cwd: &Path,
    root_override: Option<&Path>,
) -> Result<WorkspaceLayout, OrbitError> {
    let explicit_root = explicit_orbit_root(cwd, root_override);
    let explicit_root = explicit_root.as_deref();
    let registry_root = registry_root(explicit_root);

    let registered_paths = registry_root
        .as_deref()
        .map(|registry_root| registered_checkout_paths(registry_root, cwd, explicit_root))
        .transpose()?
        .flatten();
    let (repo_root, orbit_root) = match registered_paths {
        Some(paths) => paths,
        None => unregistered_checkout_paths(cwd, explicit_root)?,
    };
    let workspace_id = registry_root
        .as_deref()
        .and_then(|registry_root| registered_workspace_id(registry_root, &repo_root));

    Ok(WorkspaceLayout {
        repo_root,
        orbit_root,
        workspace_id,
    })
}

/// The explicitly selected Orbit data root.
///
/// `--root` first, then the `ORBIT_ROOT` escape hatch, matching the precedence
/// every other root-aware surface applies. Honoring the variable here is what
/// keeps `ORBIT_ROOT=<root> orbit mcp init` pointed at the same workspace as
/// `ORBIT_ROOT=<root> orbit task list`.
fn explicit_orbit_root(cwd: &Path, root_override: Option<&Path>) -> Option<PathBuf> {
    let root = match root_override {
        Some(root) => root.to_path_buf(),
        None => {
            let value = env::var("ORBIT_ROOT").ok()?;
            let value = value.trim();
            if value.is_empty() {
                return None;
            }
            PathBuf::from(value)
        }
    };
    Some(if root.is_relative() {
        cwd.join(root)
    } else {
        root
    })
}

/// The Orbit data root whose workspace catalog describes this checkout.
///
/// An explicit root wins when it actually holds a catalog — that is the
/// external-root layout, and a checkout it does not list must not be resolved
/// out of the machine-global catalog behind the operator's back. A repo-local
/// `.orbit` passed as `--root` holds no catalog of its own, so the machine's
/// global root answers for it.
fn registry_root(explicit_root: Option<&Path>) -> Option<PathBuf> {
    if let Some(root) = explicit_root.filter(|root| has_registry(root)) {
        return Some(root.to_path_buf());
    }
    global_root_for(None).ok().filter(|root| has_registry(root))
}

fn has_registry(global_root: &Path) -> bool {
    workspace_registry::registry_path_for(global_root).is_file()
}

fn registered_checkout_paths(
    registry_root: &Path,
    cwd: &Path,
    explicit_root: Option<&Path>,
) -> Result<Option<(PathBuf, PathBuf)>, OrbitError> {
    let Some(registry) = load_registry(registry_root) else {
        return Ok(None);
    };
    let checkout = match explicit_root {
        Some(root) => sole_checkout_for_root(&registry, root)
            .ok_or_else(|| explicit_root_resolution_error(&registry, root))?,
        None => {
            let Some(checkout) = checkout_for_cwd(&registry, cwd) else {
                return Ok(None);
            };
            checkout
        }
    };
    Ok(Some((
        checkout.repo_root.clone(),
        checkout.orbit_dir.clone(),
    )))
}

fn load_registry(global_root: &Path) -> Option<WorkspaceRegistry> {
    workspace_registry::load_registry_from(&workspace_registry::registry_path_for(global_root)).ok()
}

fn explicit_root_resolution_error(registry: &WorkspaceRegistry, root: &Path) -> OrbitError {
    let candidates = registry
        .checkouts
        .iter()
        .map(|checkout| {
            format!(
                "{} (Orbit root {})",
                checkout.repo_root.display(),
                checkout.orbit_dir.display()
            )
        })
        .collect::<Vec<_>>();
    let candidates = if candidates.is_empty() {
        "none".to_string()
    } else {
        candidates.join(", ")
    };

    OrbitError::InvalidInput(format!(
        "explicit Orbit root '{}' does not identify exactly one registered checkout; candidate checkouts: {}",
        root.display(),
        candidates
    ))
}

fn checkout_for_cwd<'a>(
    registry: &'a WorkspaceRegistry,
    cwd: &Path,
) -> Option<&'a WorkspaceCheckout> {
    workspace_registry::find_checkout_by_path(registry, cwd).or_else(|| {
        let canonical = fs::canonicalize(cwd).ok()?;
        workspace_registry::find_checkout_by_path(registry, &canonical)
    })
}

/// The checkout an explicit `--root` identifies on its own, used when the
/// current directory is not inside a registered checkout.
///
/// `--root` names a data directory, not a checkout, so it can only name a
/// checkout when exactly one registered checkout keeps its state there. Several
/// checkouts sharing one root stay ambiguous and resolution fails rather than
/// writing into an arbitrary one.
fn sole_checkout_for_root<'a>(
    registry: &'a WorkspaceRegistry,
    root: &Path,
) -> Option<&'a WorkspaceCheckout> {
    let root = canonical_or_original(root);
    let mut matching = registry
        .checkouts
        .iter()
        .filter(|checkout| canonical_or_original(&checkout.orbit_dir) == root);
    let first = matching.next()?;
    matching.next().is_none().then_some(first)
}

/// Locate a checkout that no registry knows about.
///
/// An explicit `--root` is only usable here when it is a repo-local `.orbit`
/// directory, whose parent is the checkout by construction. Any other
/// unregistered root has no derivable checkout, and reporting that is the point
/// — the alternative is a silent write into a directory that is neither the
/// repository nor the Orbit root.
fn unregistered_checkout_paths(
    cwd: &Path,
    root_override: Option<&Path>,
) -> Result<(PathBuf, PathBuf), OrbitError> {
    let Some(root) = root_override else {
        return walk_up_checkout_paths(cwd);
    };

    let repo_root = root
        .file_name()
        .filter(|name| *name == ".orbit")
        .and_then(|_| root.parent())
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "`--root {}` does not identify exactly one registered checkout; run this command from inside the checkout, or register it with `orbit workspace init` first",
                root.display()
            ))
        })?;
    Ok((repo_root.to_path_buf(), root.to_path_buf()))
}

fn walk_up_checkout_paths(cwd: &Path) -> Result<(PathBuf, PathBuf), OrbitError> {
    if cwd.file_name().is_some_and(|name| name == ".orbit") && cwd.is_dir() {
        return Ok((cwd.parent().unwrap_or(cwd).to_path_buf(), cwd.to_path_buf()));
    }

    // Skip the user's global $HOME/.orbit during ancestor walk-up. It is the
    // global Orbit root, not a workspace, so adopting it would silently write
    // workspace-scope MCP configs to home-scope paths.
    let home = env_home_dir();
    for ancestor in cwd.ancestors() {
        let orbit_root = ancestor.join(".orbit");
        if orbit_root.is_dir() && !is_global_orbit_dir(&orbit_root) {
            return Ok((ancestor.to_path_buf(), orbit_root));
        }
        if home
            .as_deref()
            .is_some_and(|home| paths_equivalent(ancestor, home))
        {
            break;
        }
    }

    Err(OrbitError::InvalidInput(
        "current directory is not inside an initialized Orbit workspace; run `orbit workspace init` first or pass `--root <path/to/.orbit>`".to_string(),
    ))
}

/// The logical workspace ID this checkout is registered under in `global_root`,
/// or `None` when that registry does not know it.
///
/// This is what a generated integration binds its MCP server to. The logical
/// `ws_*` ID is used rather than the checkout path so a linked worktree — whose
/// own checkout identity may have diverged from the registration — still names
/// one workspace, and so a config that travels with the repo does not carry an
/// absolute path from the machine that wrote it.
///
/// An unregistered checkout is not an error here: the generated server simply
/// stays unbound, exactly as it was before a binding existed, and every
/// workspace-scoped call supplies its own selector.
fn registered_workspace_id(global_root: &Path, repo_root: &Path) -> Option<String> {
    let selector = repo_root.to_str()?;
    RegisteredRuntimeFactory::resolve_workspace_selector(global_root, selector)
        .ok()
        .map(|selected| selected.workspace.id)
}

fn is_global_orbit_dir(candidate: &Path) -> bool {
    let Ok(global) = workspace_registry::global_orbit_dir() else {
        return false;
    };
    paths_equivalent(candidate, &global)
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    left == right || canonical_or_original(left) == canonical_or_original(right)
}

fn canonical_or_original(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub(super) fn env_home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("USERPROFILE")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
}
