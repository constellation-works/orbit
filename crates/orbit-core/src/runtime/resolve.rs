use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use serde::Deserialize;

use crate::paths;
use crate::runtime::run_input::managed_run_context_from_env;

/// Registry locator emitted to children of an Orbit-managed run.
///
/// Unlike `ORBIT_ROOT`, this selects only the host-global registry and never
/// changes workspace root discovery or bootstrap ownership.
const ORBIT_REGISTRY_ROOT_ENV: &str = "ORBIT_REGISTRY_ROOT";

/// Returns the global Orbit root.
///
/// Direct commands use `~/.orbit/`. A trusted managed child may instead carry
/// the dispatching host's registry locator; this never selects its workspace.
pub fn resolve_global_root() -> Result<PathBuf, OrbitError> {
    if managed_run_context_from_env()
        && let Ok(value) = std::env::var(ORBIT_REGISTRY_ROOT_ENV)
        && !value.trim().is_empty()
    {
        let root = PathBuf::from(value);
        if !root.is_absolute() {
            return Err(OrbitError::InvalidInput(format!(
                "{ORBIT_REGISTRY_ROOT_ENV} must be an absolute path inside an Orbit-managed run"
            )));
        }
        return Ok(root);
    }
    orbit_common::fs::path::global_orbit_dir()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOrbitRoots {
    /// Shared workspace `.orbit` directory (the main-worktree root).
    pub shared_root: PathBuf,
    /// Worktree-local `.orbit` directory (equals `shared_root` outside linked
    /// worktrees).
    pub local_root: PathBuf,
}

/// Registry-neutral root candidate supplied by a higher-level workspace
/// catalog. Resolution applies it at the historical path-override precedence:
/// after linked-worktree discovery and before filesystem walk-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRootHint {
    pub orbit_dir: PathBuf,
}

impl ResolvedOrbitRoots {
    fn pinned(root: PathBuf) -> Self {
        Self {
            shared_root: root.clone(),
            local_root: root,
        }
    }

    fn new(shared_root: PathBuf, local_root: PathBuf) -> Self {
        Self {
            shared_root,
            local_root,
        }
    }
}

/// Resolves the `.orbit` shared and local roots using the full resolution chain.
///
/// Linked git worktrees intentionally keep `shared_root` pointed at the main
/// checkout's `.orbit` before local walk-up discovery, so existing task state
/// cannot diverge per worktree. They also expose a separate `local_root` at the
/// current linked checkout's `.orbit` for later per-worktree artifacts.
/// Explicit `--root` and `ORBIT_ROOT` still take precedence over this automatic
/// worktree resolution and pin both roots. A worktree-local `.orbit/` is not
/// consumed by any existing store in this phase.
///
/// Resolution order:
/// 1. `--root` flag (escape hatch)
/// 2. `ORBIT_ROOT` env (escape hatch)
/// 3. Linked git worktree's main checkout `.orbit/` as `shared_root`
/// 4. `path_overrides` in global registry (longest prefix match from cwd)
/// 5. Walk up from cwd to find first workspace `.orbit/` directory, skipping
///    the global home `.orbit/`
/// 6. Legacy: git repo root (for repos without `.orbit/` directory yet),
///    skipping the global home `.orbit/`
/// 7. Fallback: `<cwd>/.orbit`, refusing if it would resolve to the global
///    home `.orbit/`
pub(crate) fn resolve_initialize_roots(
    cwd: &Path,
    root_override: Option<&Path>,
) -> Result<ResolvedOrbitRoots, OrbitError> {
    resolve_initialize_roots_with_hint(cwd, root_override, None)
}

/// Resolve initialized roots using a caller-supplied workspace catalog hint.
pub fn resolve_initialize_roots_with_hint(
    cwd: &Path,
    root_override: Option<&Path>,
    hint: Option<&WorkspaceRootHint>,
) -> Result<ResolvedOrbitRoots, OrbitError> {
    resolve_roots(
        cwd,
        root_override,
        ExplicitRootMode::RequireInitialized,
        hint,
    )
}

/// Resolves `.orbit` roots for commands that are allowed to create them.
pub(crate) fn resolve_bootstrap_roots(
    cwd: &Path,
    root_override: Option<&Path>,
) -> Result<ResolvedOrbitRoots, OrbitError> {
    resolve_bootstrap_roots_with_hint(cwd, root_override, None)
}

/// Resolve bootstrap roots using a caller-supplied workspace catalog hint.
pub fn resolve_bootstrap_roots_with_hint(
    cwd: &Path,
    root_override: Option<&Path>,
    hint: Option<&WorkspaceRootHint>,
) -> Result<ResolvedOrbitRoots, OrbitError> {
    resolve_roots(
        cwd,
        root_override,
        ExplicitRootMode::AllowUninitialized,
        hint,
    )
}

/// Core implementation for `.orbit` workspace root discovery.
///
/// Explicit roots from `--root` and `ORBIT_ROOT` win first. Linked git
/// worktrees then resolve shared state through the main checkout while exposing
/// the linked checkout's local `.orbit/`, before registry overrides and legacy
/// walk-up run.
fn resolve_roots(
    cwd: &Path,
    root_override: Option<&Path>,
    explicit_root_mode: ExplicitRootMode,
    hint: Option<&WorkspaceRootHint>,
) -> Result<ResolvedOrbitRoots, OrbitError> {
    // 1. --root flag (escape hatch)
    if let Some(root) = root_override {
        let root =
            resolve_explicit_root_path_value(&root.to_string_lossy(), cwd, explicit_root_mode)?;
        return Ok(log_resolved_roots(
            cwd,
            "explicit_root",
            ResolvedOrbitRoots::pinned(root),
        ));
    }

    // 2. ORBIT_ROOT env (escape hatch)
    if let Ok(explicit) = std::env::var("ORBIT_ROOT")
        && !explicit.trim().is_empty()
    {
        let root = resolve_explicit_root_path_value(&explicit, cwd, explicit_root_mode)?;
        return Ok(log_resolved_roots(
            cwd,
            "orbit_root_env",
            ResolvedOrbitRoots::pinned(root),
        ));
    }

    // 3. Linked git worktree's main checkout .orbit/ as shared_root
    if let Some(orbit_dir) = find_main_worktree_orbit_dir(cwd) {
        let shared_root = resolve_orbit_dir_candidate(&orbit_dir)?;
        let local_root = local_worktree_orbit_dir(cwd);
        return Ok(log_resolved_roots(
            cwd,
            "git_worktree_main",
            ResolvedOrbitRoots::new(shared_root, local_root),
        ));
    }

    // 4. caller-supplied catalog hint (legacy adapter uses path_overrides)
    if let Some(ws) = hint.map(|hint| hint.orbit_dir.clone()) {
        return Ok(log_resolved_roots(
            cwd,
            "path_override",
            ResolvedOrbitRoots::pinned(ws),
        ));
    }

    // 5. Walk up from cwd to find first workspace .orbit/ directory. Bootstrap
    // flows stop at the current git root, or cwd when there is no git root, so
    // unrelated parent directories with `.orbit/` cannot capture new workspaces.
    let walk_up_boundaries = walk_up_boundaries(cwd, explicit_root_mode);
    if let Some(orbit_dir) = find_orbit_dir_walk_up(cwd, &walk_up_boundaries) {
        let root = resolve_orbit_dir_candidate(&orbit_dir)?;
        return Ok(log_resolved_roots(
            cwd,
            "walk_up",
            ResolvedOrbitRoots::pinned(root),
        ));
    }

    // 6. Legacy: git repo root (for repos without .orbit/ directory yet).
    //    Skip when the candidate equals the global $HOME/.orbit — that happens
    //    when $HOME is itself a git repo (e.g. yadm/chezmoi/vcsh dotfile
    //    managers), and adopting the global root as a workspace would silently
    //    corrupt user state.
    if let Some(repo_root) = paths::find_git_repo_root(cwd) {
        let candidate = repo_root.join(".orbit");
        if !is_global_orbit_root(&candidate) {
            return Ok(log_resolved_roots(
                cwd,
                "git_repo_root",
                ResolvedOrbitRoots::pinned(candidate),
            ));
        }
    }

    // 7. Fallback: <cwd>/.orbit, but never the global $HOME/.orbit.
    let cwd_root = paths::cwd_orbit_root(cwd);
    if is_global_orbit_root(&cwd_root) {
        return Err(OrbitError::InvalidInput(format!(
            "{} is the global Orbit root, not a workspace; run `orbit workspace init` from inside a project directory or pass `--root <path/to/.orbit>`",
            cwd_root.display()
        )));
    }
    Ok(log_resolved_roots(
        cwd,
        "cwd_fallback",
        ResolvedOrbitRoots::pinned(cwd_root),
    ))
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum ExplicitRootMode {
    AllowUninitialized,
    RequireInitialized,
}

fn find_main_worktree_orbit_dir(cwd: &Path) -> Option<PathBuf> {
    Some(paths::find_git_main_worktree_root(cwd)?.join(".orbit"))
}

fn local_worktree_orbit_dir(cwd: &Path) -> PathBuf {
    let worktree_root = paths::find_git_worktree_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    paths::normalize_path_components(&worktree_root.join(".orbit"))
}

/// Walks up the directory tree from `start` looking for the first workspace
/// `.orbit/` directory.
///
/// The user's global `$HOME/.orbit` is not a workspace root. Without this guard,
/// `orbit workspace init` in a repo under `$HOME` with no local `.orbit/` would
/// discover the global root before the git-repo bootstrap fallback and then
/// write workspace state into `$HOME/.orbit`.
fn find_orbit_dir_walk_up(start: &Path, boundaries: &[PathBuf]) -> Option<PathBuf> {
    let mut current = start;
    loop {
        let candidate = current.join(".orbit");
        if candidate.is_dir() && !is_global_orbit_root(&candidate) {
            return Some(candidate);
        }
        if boundaries
            .iter()
            .any(|boundary| paths_equivalent(current, boundary))
        {
            return None;
        }
        current = current.parent()?;
    }
}

fn walk_up_boundaries(cwd: &Path, explicit_root_mode: ExplicitRootMode) -> Vec<PathBuf> {
    let mut boundaries = Vec::new();
    if explicit_root_mode == ExplicitRootMode::AllowUninitialized {
        boundaries.push(paths::find_git_worktree_root(cwd).unwrap_or_else(|| cwd.to_path_buf()));
    }
    if let Some(home) = home_dir_boundary()
        && !boundaries
            .iter()
            .any(|boundary| paths_equivalent(boundary, &home))
    {
        boundaries.push(home);
    }
    boundaries
}

fn home_dir_boundary() -> Option<PathBuf> {
    paths::home_dir()
}

/// Whether `candidate` resolves to the global Orbit root (`$HOME/.orbit`,
/// or `%USERPROFILE%\.orbit`). This is the single source of truth for
/// "is this root the global one" — anything that needs to make a decision
/// contingent on that (whether to touch home-scoped skill link
/// directories, what path to report to the user) should call this rather
/// than re-deriving the comparison.
pub fn is_global_orbit_root(candidate: &Path) -> bool {
    let Some(global) = paths::home_dir().map(|home| home.join(".orbit")) else {
        return false;
    };
    paths_equivalent(candidate, &global)
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }

    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

fn resolve_orbit_dir_candidate(orbit_dir: &Path) -> Result<PathBuf, OrbitError> {
    let config_path = orbit_dir.join("config.toml");
    if config_path.exists()
        && let Some(configured_root) = configured_root_from_config(&config_path)?
    {
        return Ok(configured_root);
    }
    Ok(orbit_dir.to_path_buf())
}

fn log_resolved_roots(
    cwd: &Path,
    source: &'static str,
    roots: ResolvedOrbitRoots,
) -> ResolvedOrbitRoots {
    tracing::debug!(
        source,
        cwd = %cwd.display(),
        shared_root = %roots.shared_root.display(),
        local_root = %roots.local_root.display(),
        "resolved Orbit roots"
    );
    roots
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RootField {
    String(String),
    Table { path: String },
}

#[derive(Debug, Deserialize)]
struct RootOnlyConfig {
    root: Option<RootField>,
}

fn configured_root_from_config(config_path: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let raw = fs::read_to_string(config_path).map_err(|err| {
        OrbitError::Io(format!(
            "failed to read runtime config '{}': {err}",
            config_path.display()
        ))
    })?;
    let parsed = toml::from_str::<RootOnlyConfig>(&raw).map_err(|err| {
        OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': {err}",
            config_path.display()
        ))
    })?;
    let Some(root_value) = parsed.root else {
        return Ok(None);
    };
    let root_value = match root_value {
        RootField::String(value) => value,
        RootField::Table { path } => path,
    };
    let base = config_path.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "invalid config path without parent: {}",
            config_path.display()
        ))
    })?;
    Ok(Some(resolve_root_path_value(&root_value, base)?))
}

fn resolve_explicit_root_path_value(
    raw: &str,
    base_dir: &Path,
    mode: ExplicitRootMode,
) -> Result<PathBuf, OrbitError> {
    let root = resolve_root_path_value(raw, base_dir)?;
    match mode {
        ExplicitRootMode::AllowUninitialized => Ok(root),
        ExplicitRootMode::RequireInitialized => resolve_initialized_root(root),
    }
}

fn resolve_initialized_root(root: PathBuf) -> Result<PathBuf, OrbitError> {
    let child_orbit = root.join(".orbit");
    if is_initialized_orbit_root(&child_orbit) {
        return Ok(child_orbit);
    }

    if is_initialized_orbit_root(&root) {
        return Ok(root);
    }

    Err(OrbitError::InvalidInput(format!(
        "{} is not an Orbit workspace; run `orbit workspace init` first or pass `--root <path/to/.orbit>`",
        root.display()
    )))
}

fn is_initialized_orbit_root(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    if path.join("config.toml").is_file() {
        return true;
    }

    // Task bundles are canonical in the global registry. The checkout-local
    // `.orbit/tasks` projection was removed in layout version 3, so it cannot
    // be part of the initialized-root probe anymore.
    path.join("resources").is_dir() && path.join("state").is_dir()
}

fn resolve_root_path_value(raw: &str, base_dir: &Path) -> Result<PathBuf, OrbitError> {
    paths::resolve_path_value(raw, base_dir, "root path")
}

/// Like [`resolve_initialize_roots`] but never falls through to the
/// `<cwd>/.orbit` bootstrap fallback. Returns `Ok(None)` when no initialized
/// workspace is discovered anywhere in the chain.
///
/// Explicit roots (`--root`, `ORBIT_ROOT`) keep their `RequireInitialized`
/// semantics: pointing at an uninitialized path is still a hard error, since
/// the user explicitly asked for that root.
pub fn try_resolve_initialized_roots(
    cwd: &Path,
    root_override: Option<&Path>,
) -> Result<Option<ResolvedOrbitRoots>, OrbitError> {
    try_resolve_initialized_roots_with_hint(cwd, root_override, None)
}

/// Resolve an existing workspace using a caller-supplied catalog hint without
/// reading the legacy registry compatibility surface.
pub fn try_resolve_initialized_roots_with_hint(
    cwd: &Path,
    root_override: Option<&Path>,
    hint: Option<&WorkspaceRootHint>,
) -> Result<Option<ResolvedOrbitRoots>, OrbitError> {
    if let Some(root) = root_override {
        let root = resolve_explicit_root_path_value(
            &root.to_string_lossy(),
            cwd,
            ExplicitRootMode::RequireInitialized,
        )?;
        return Ok(Some(log_resolved_roots(
            cwd,
            "explicit_root",
            ResolvedOrbitRoots::pinned(root),
        )));
    }

    if let Ok(explicit) = std::env::var("ORBIT_ROOT")
        && !explicit.trim().is_empty()
    {
        let root =
            resolve_explicit_root_path_value(&explicit, cwd, ExplicitRootMode::RequireInitialized)?;
        return Ok(Some(log_resolved_roots(
            cwd,
            "orbit_root_env",
            ResolvedOrbitRoots::pinned(root),
        )));
    }

    if let Some(orbit_dir) = find_main_worktree_orbit_dir(cwd)
        && is_initialized_orbit_root(&orbit_dir)
    {
        let shared_root = resolve_orbit_dir_candidate(&orbit_dir)?;
        let local_root = local_worktree_orbit_dir(cwd);
        return Ok(Some(log_resolved_roots(
            cwd,
            "git_worktree_main",
            ResolvedOrbitRoots::new(shared_root, local_root),
        )));
    }

    if let Some(ws) = hint.map(|hint| hint.orbit_dir.clone())
        && is_initialized_orbit_root(&ws)
    {
        return Ok(Some(log_resolved_roots(
            cwd,
            "path_override",
            ResolvedOrbitRoots::pinned(ws),
        )));
    }

    let walk_up_boundaries = walk_up_boundaries(cwd, ExplicitRootMode::RequireInitialized);
    if let Some(orbit_dir) = find_orbit_dir_walk_up(cwd, &walk_up_boundaries)
        && is_initialized_orbit_root(&orbit_dir)
    {
        let root = resolve_orbit_dir_candidate(&orbit_dir)?;
        return Ok(Some(log_resolved_roots(
            cwd,
            "walk_up",
            ResolvedOrbitRoots::pinned(root),
        )));
    }

    Ok(None)
}
