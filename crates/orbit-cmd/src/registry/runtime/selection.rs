use super::*;

pub(crate) fn retry_pipeline_worker_bootstrap<T>(
    mut bootstrap: impl FnMut() -> Result<T, OrbitError>,
    timeout: Duration,
    retry_interval: Duration,
) -> Result<T, OrbitError> {
    let deadline = Instant::now() + timeout;
    loop {
        match bootstrap() {
            Ok(runtime) => return Ok(runtime),
            Err(error) if error.sqlite_contention().is_some() && Instant::now() < deadline => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                thread::sleep(retry_interval.min(remaining));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Read the common no-maintenance case without taking a write lock. If the
/// snapshot needs migration or checkout validation, re-read it under the lock
/// so a concurrent registration cannot be overwritten by the maintenance save.
pub(super) fn load_registry_for_selector_resolution(
    registry_path: &Path,
    identity: &MachineIdentityState,
) -> Result<WorkspaceRegistry, OrbitError> {
    let loaded =
        workspace_registry::load_registry_from_read_only_with_machine(registry_path, identity)?;
    let mut registry = loaded.registry;
    let validation_required = workspace_registry::validate_workspaces(&mut registry);
    if !loaded.migration_required && !validation_required {
        return Ok(registry);
    }

    workspace_registry::with_registry_lock(registry_path, || {
        let mut registry =
            workspace_registry::load_registry_from_with_machine(registry_path, identity)?;
        if workspace_registry::validate_workspaces(&mut registry) {
            let _ = workspace_registry::save_registry_to(&registry, registry_path);
        }
        Ok(registry)
    })
}

/// The host data directory a registry lookup reads: the `--root` override when
/// one was passed, the trusted managed registry locator for a managed child,
/// and `~/.orbit` otherwise. Never derived from cwd, so a registry-first
/// command works from any directory.
///
/// This is the single answer to "which registry does `--root` select?", shared
/// by every root-aware surface — including the dashboard, which used to load
/// the machine-global registry unconditionally (ORB-11388).
pub fn global_root_for(root_override: Option<&Path>) -> Result<PathBuf, OrbitError> {
    match root_override {
        Some(root) => Ok(root.to_path_buf()),
        None => orbit_core::runtime::resolve_global_root(),
    }
}

/// Project the server host's task namespace before Core opens a selected
/// workspace runtime.
pub fn sync_runtime_task_prefix(global_root: &Path) -> Result<(), OrbitError> {
    sync_task_prefix(global_root)
}

pub(super) enum CliWorkspaceTarget<'a> {
    CurrentRuntime,
    Checkout {
        workspace: &'a Workspace,
        checkout: &'a WorkspaceCheckout,
        rewrite_to_repo_root: bool,
        local_root: PathBuf,
    },
}

pub(super) fn cli_workspace_selector(input: &Value) -> Result<Option<String>, OrbitError> {
    match input.get("workspace") {
        None => Ok(None),
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            Ok(Some(trimmed.to_string()))
        }
        Some(_) => Err(OrbitError::InvalidInput(
            "`workspace` must be a string".to_string(),
        )),
    }
}

pub(super) fn resolve_cli_workspace_target<'a>(
    registry: &'a WorkspaceRegistry,
    runtime: &OrbitRuntime,
    selector: &str,
    local_machine_id: Option<&str>,
) -> Result<CliWorkspaceTarget<'a>, OrbitError> {
    let selector = local_workspace_selector(selector, local_machine_id)?;
    if selector_looks_like_path(selector) {
        return resolve_cli_workspace_path(registry, Some(runtime), selector);
    }
    let (workspace, checkout) = resolve_named_cli_checkout(registry, selector)?;
    Ok(CliWorkspaceTarget::Checkout {
        workspace,
        checkout,
        rewrite_to_repo_root: true,
        local_root: checkout.orbit_dir.clone(),
    })
}

pub(super) fn resolve_cli_workspace_binding<'a>(
    registry: &'a WorkspaceRegistry,
    selector: &str,
    local_machine_id: Option<&str>,
) -> Result<(&'a Workspace, &'a WorkspaceCheckout, PathBuf), OrbitError> {
    let selector = local_workspace_selector(selector, local_machine_id)?;
    match if selector_looks_like_path(selector) {
        resolve_cli_workspace_path(registry, None, selector)?
    } else {
        let (workspace, checkout) = resolve_named_cli_checkout(registry, selector)?;
        CliWorkspaceTarget::Checkout {
            workspace,
            checkout,
            local_root: checkout.orbit_dir.clone(),
            rewrite_to_repo_root: true,
        }
    } {
        CliWorkspaceTarget::Checkout {
            workspace,
            checkout,
            local_root,
            ..
        } => Ok((workspace, checkout, local_root)),
        CliWorkspaceTarget::CurrentRuntime => Err(unsupported_cli_workspace(selector)),
    }
}

/// Federation advertises `hm_*/ws_*` selectors. The local CLI can resolve
/// only this machine's member; a foreign host must not be treated as a path.
fn local_workspace_selector<'a>(
    selector: &'a str,
    local_machine_id: Option<&str>,
) -> Result<&'a str, OrbitError> {
    let Some((host, workspace)) = selector.split_once('/') else {
        return Ok(selector);
    };
    if !host.starts_with("hm_") || !workspace.starts_with("ws_") || workspace.contains('/') {
        return Ok(selector);
    }
    if local_machine_id == Some(host) {
        return Ok(workspace);
    }
    Err(OrbitError::InvalidInput(format!(
        "workspace selector '{selector}' belongs to host '{host}', not this local host{}",
        local_machine_id
            .map(|id| format!(" '{id}'"))
            .unwrap_or_default()
    )))
}

pub(super) fn resolve_named_cli_checkout<'a>(
    registry: &'a WorkspaceRegistry,
    selector: &str,
) -> Result<(&'a Workspace, &'a WorkspaceCheckout), OrbitError> {
    let workspace = workspace_registry::resolve_logical_workspace(registry, selector)?;
    let checkout = registry
        .checkouts
        .iter()
        .find(|checkout| checkout.workspace_id == workspace.id)
        .ok_or_else(|| unsupported_cli_workspace(selector))?;
    Ok((workspace, checkout))
}

pub(super) fn resolve_cli_workspace_path<'a>(
    registry: &'a WorkspaceRegistry,
    runtime: Option<&OrbitRuntime>,
    selector: &str,
) -> Result<CliWorkspaceTarget<'a>, OrbitError> {
    let raw = Path::new(selector);
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(raw)
    };
    if let Ok(canonical) = candidate.canonicalize()
        && canonical.is_dir()
    {
        if let Some(checkout) = find_checkout_for_canonical_path(registry, &canonical)
            .or_else(|| find_checkout_for_git_common_dir(registry, &canonical))
        {
            let workspace =
                workspace_registry::find_workspace_by_id(registry, &checkout.workspace_id)
                    .ok_or_else(|| unsupported_cli_workspace(selector))?;
            return Ok(CliWorkspaceTarget::Checkout {
                workspace,
                checkout,
                rewrite_to_repo_root: false,
                local_root: local_root_for_selected_path(checkout, &canonical),
            });
        }
        if let Some(runtime) = runtime
            && path_is_inside(&runtime.paths().repo_root, &canonical)
        {
            return Ok(CliWorkspaceTarget::CurrentRuntime);
        }
    }
    if let Some(checkout) = find_checkout_for_raw_path(registry, &candidate) {
        let workspace = workspace_registry::find_workspace_by_id(registry, &checkout.workspace_id)
            .ok_or_else(|| unsupported_cli_workspace(selector))?;
        return Ok(CliWorkspaceTarget::Checkout {
            workspace,
            checkout,
            rewrite_to_repo_root: false,
            local_root: checkout.orbit_dir.clone(),
        });
    }
    Err(unsupported_cli_workspace(selector))
}

pub(super) fn find_checkout_for_canonical_path<'a>(
    registry: &'a WorkspaceRegistry,
    canonical: &Path,
) -> Option<&'a WorkspaceCheckout> {
    registry.checkouts.iter().find(|checkout| {
        canonical_path(&checkout.repo_root) == canonical
            || canonical_path(&checkout.orbit_dir) == canonical
            || checkout
                .path_overrides
                .iter()
                .any(|override_path| canonical_path(override_path) == canonical)
    })
}

pub(super) fn find_checkout_for_raw_path<'a>(
    registry: &'a WorkspaceRegistry,
    candidate: &Path,
) -> Option<&'a WorkspaceCheckout> {
    let normalized = normalize_path(candidate);
    find_checkout_for_canonical_path(registry, &normalized)
        .or_else(|| workspace_registry::find_checkout_by_path(registry, &normalized))
}

pub(super) fn find_checkout_for_git_common_dir<'a>(
    registry: &'a WorkspaceRegistry,
    selected: &Path,
) -> Option<&'a WorkspaceCheckout> {
    let selected_common = git_common_dir(selected)?;
    let mut recorded = registry.checkouts.iter().filter(|checkout| {
        recorded_git_common_dir(checkout).is_some_and(|common| common == selected_common)
    });
    if let Some(first) = recorded.next() {
        return recorded.next().is_none().then_some(first);
    }
    // Recorded `.git` missed every checkout (for example a gitfile worktree
    // registered as the catalog checkout). Spawn only on that zero-hit path.
    let mut spawned = registry.checkouts.iter().filter(|checkout| {
        git_common_dir(&checkout.repo_root).is_some_and(|common| common == selected_common)
    });
    let first = spawned.next()?;
    spawned.next().is_none().then_some(first)
}

/// The Git common dir recorded for a catalog checkout: `{orbit_dir}/../.git`
/// when that path is a directory. Linked-worktree gitfiles are not the common
/// dir and return `None` so the caller can fall back to a git spawn.
pub(super) fn recorded_git_common_dir(checkout: &WorkspaceCheckout) -> Option<PathBuf> {
    let git_dir = checkout.orbit_dir.parent()?.join(".git");
    git_dir.is_dir().then(|| canonical_path(&git_dir))
}

pub(super) fn git_common_dir(path: &Path) -> Option<PathBuf> {
    #[cfg(test)]
    GIT_PROCESS_SPAWNS.with(|count| count.set(count.get() + 1));

    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    let trimmed = raw.lines().next()?.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(canonical_path(Path::new(trimmed)))
}

/// Keep registered/shared state on the catalog checkout while choosing the
/// Git-versioned local root of an explicitly selected linked checkout.
pub(super) fn local_root_for_selected_path(
    checkout: &WorkspaceCheckout,
    selected: &Path,
) -> PathBuf {
    if canonical_path(&checkout.orbit_dir) == selected {
        return checkout.orbit_dir.clone();
    }
    git_workdir_root(selected)
        .map(|root| root.join(".orbit"))
        .unwrap_or_else(|| checkout.orbit_dir.clone())
}

/// Runtime `local_root` for a selector open.
///
/// Standing in a Git-linked worktree of the selected checkout uses that
/// worktree's `.orbit` for Git-versioned definitions on both read-only and
/// writable opens, while `shared_root` stays the registered primary. Otherwise
/// read-only opens keep an explicit linked-path candidate root, and writable
/// opens keep the registered primary so a linked path selector from another
/// cwd cannot retarget mutations [ORB-12665].
pub(super) fn local_root_for_runtime_open(
    selected: &ResolvedWorkspaceSelection,
    cwd: &Path,
    read_only: bool,
) -> PathBuf {
    if let Some(local_root) = linked_worktree_local_root(&selected.checkout, cwd) {
        return local_root;
    }
    if read_only {
        selected.local_root.clone()
    } else {
        selected.checkout.orbit_dir.clone()
    }
}

/// `.orbit` of `cwd` when it is a Git-linked worktree of `checkout`.
///
/// Returns `None` when cwd is the registered checkout itself, outside Git, or
/// a checkout of another repository. Managed jrun worktrees live under
/// `<repo>/.orbit/state/worktrees/**`, so a path-prefix test is not enough;
/// Git's common directory is the identity.
pub(super) fn linked_worktree_local_root(
    checkout: &WorkspaceCheckout,
    cwd: &Path,
) -> Option<PathBuf> {
    let worktree = git_workdir_root(cwd)?;
    if canonical_path(&worktree) == canonical_path(&checkout.repo_root) {
        return None;
    }
    let worktree_common = git_common_dir(&worktree)?;
    let registered_common =
        recorded_git_common_dir(checkout).or_else(|| git_common_dir(&checkout.repo_root))?;
    (worktree_common == registered_common).then(|| worktree.join(".orbit"))
}

/// Worktree root of `path` from the filesystem `.git` marker, without spawning
/// git. A linked worktree's `.git` is a file; a primary checkout's is a directory.
pub(super) fn git_workdir_root(path: &Path) -> Option<PathBuf> {
    path.ancestors().find_map(|ancestor| {
        let git_marker = ancestor.join(".git");
        (git_marker.is_dir() || git_marker.is_file()).then(|| canonical_path(ancestor))
    })
}

#[cfg(test)]
thread_local! {
    static GIT_PROCESS_SPAWNS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) struct GitProcessProbes;

#[cfg(test)]
impl GitProcessProbes {
    pub(crate) fn capture() -> Self {
        GIT_PROCESS_SPAWNS.with(|count| count.set(0));
        Self
    }

    pub(crate) fn git_process_spawns(&self) -> usize {
        GIT_PROCESS_SPAWNS.with(Cell::get)
    }
}

pub(super) fn same_cli_checkout(runtime: &OrbitRuntime, checkout: &WorkspaceCheckout) -> bool {
    canonical_path(&runtime.paths().repo_root) == canonical_path(&checkout.repo_root)
}

pub(super) fn path_is_inside(parent: &Path, child: &Path) -> bool {
    child.starts_with(canonical_path(parent))
}

pub(super) fn canonical_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Whether a workspace selector is a checkout path rather than a registered
/// name or logical ID. One owner for that classification: a bare name must
/// never be silently joined to cwd and prefix-matched (ORB-11388).
pub fn selector_looks_like_path(selector: &str) -> bool {
    let path = Path::new(selector);
    path.is_absolute()
        || selector == "."
        || selector == ".."
        || selector.contains('/')
        || selector.contains('\\')
}

pub(super) fn set_input_workspace(input: &mut Value, repo_root: &Path) -> Result<(), OrbitError> {
    let Some(object) = input.as_object_mut() else {
        return Err(OrbitError::InvalidInput(
            "tool input must be a JSON object".to_string(),
        ));
    };
    object.insert(
        "workspace".to_string(),
        Value::String(repo_root.to_string_lossy().into_owned()),
    );
    Ok(())
}

pub(super) fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

pub(super) fn unsupported_cli_workspace(selector: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "unknown workspace selector '{selector}'; pass a registered workspace name, a logical workspace ID, or an absolute local checkout path"
    ))
}

pub(super) fn inactive_cli_workspace(
    workspace: &Workspace,
    checkout: &WorkspaceCheckout,
) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "workspace '{}' ({}) is {} on this machine; recorded checkout path: {}",
        workspace.name,
        workspace.id,
        workspace.status,
        checkout.repo_root.display(),
    ))
}

/// The two inputs every runtime open reads before it dispatches anything: the
/// global `config.toml` `[machine]` table, which classifies this machine for
/// the task-prefix projection, the automation identity, and registry
/// validation; and `workspaces.json`, which selects the checkout. Agents shell
/// out to `orbit` hundreds of times per run, so each is read once and threaded
/// through the open rather than re-read by every step that needs it
/// [DANI-10371].
pub(super) struct RuntimeOpenInputs {
    pub(super) global_root: PathBuf,
    pub(super) identity: MachineIdentityState,
    pub(super) registry: WorkspaceRegistry,
}

impl RuntimeOpenInputs {
    pub(super) fn read(global_root: &Path) -> Result<Self, OrbitError> {
        let identity = inspect_machine_identity(global_root)?;
        let registry = workspace_registry::load_registry_from_with_machine(
            &workspace_registry::registry_path_for(global_root),
            &identity,
        )?;
        Ok(Self {
            global_root: global_root.to_path_buf(),
            identity,
            registry,
        })
    }
}

/// Project the machine-owned task namespace into the neutral task allocator.
/// Custom/legacy roots without a `[machine]` table retain the historical ORB
/// default; once an identity exists, a `machine.task_prefix` that contradicts
/// the ids already minted locally fails closed at the allocator.
pub(crate) fn sync_task_prefix(global_root: &Path) -> Result<(), OrbitError> {
    sync_task_prefix_for_identity(global_root, &inspect_machine_identity(global_root)?)
}

/// The same projection for a caller that already classified the identity.
pub(super) fn sync_task_prefix_for_identity(
    global_root: &Path,
    identity: &MachineIdentityState,
) -> Result<(), OrbitError> {
    let task_prefix = match identity {
        MachineIdentityState::Present(identity) => identity.task_prefix.clone(),
        MachineIdentityState::Absent => return Ok(()),
    };

    let registry = TaskRegistryStore::open(&task_registry_path(global_root))?;
    registry.set_task_prefix(&task_prefix)
}

pub(super) fn replica_owner_for_checkout(checkout: &WorkspaceCheckout) -> Option<String> {
    (checkout.role == Some(WorkspaceCheckoutRole::Replica))
        .then(|| checkout.owner_machine_id.clone())
        .flatten()
}

pub(super) fn workspace_root_hint(cwd: &Path) -> Option<WorkspaceRootHint> {
    let registry = workspace_registry::load_registry().ok()?;
    checkout_root_hint(&registry, cwd)
}

/// The catalog hint for `cwd` from a registry the caller already loaded.
pub(super) fn checkout_root_hint(
    registry: &WorkspaceRegistry,
    cwd: &Path,
) -> Option<WorkspaceRootHint> {
    let checkout = workspace_registry::find_checkout_by_path(registry, cwd)?;
    Some(WorkspaceRootHint {
        orbit_dir: checkout.orbit_dir.clone(),
    })
}

/// Resolve a catalog hint for a bootstrap command without crossing into a
/// nested, independently rooted Git repository.
///
/// Ordinary runtime lookup keeps longest-prefix registry semantics. Bootstrap
/// is different because it is allowed to create a workspace: an ancestor
/// checkout must not capture a new child repository before Core's Git-bounded
/// walk-up gets a chance to select `<child>/.orbit`. An explicit path override
/// rooted inside the child repository remains authoritative.
pub(super) fn bootstrap_workspace_root_hint(cwd: &Path) -> Option<WorkspaceRootHint> {
    let registry = workspace_registry::load_registry().ok()?;
    let checkout = workspace_registry::find_checkout_by_path(&registry, cwd)?;
    if checkout_crosses_nested_git_boundary(checkout, cwd) {
        return None;
    }
    Some(WorkspaceRootHint {
        orbit_dir: checkout.orbit_dir.clone(),
    })
}

pub(super) fn checkout_crosses_nested_git_boundary(
    checkout: &WorkspaceCheckout,
    cwd: &Path,
) -> bool {
    let cwd = canonical_or_original(cwd);
    let Some(git_root) = cwd.ancestors().find(|ancestor| {
        let git_marker = ancestor.join(".git");
        git_marker.is_dir() || git_marker.is_file()
    }) else {
        return false;
    };
    let git_root = canonical_or_original(git_root);

    !std::iter::once(&checkout.repo_root)
        .chain(&checkout.path_overrides)
        .map(|root| canonical_or_original(root))
        .any(|root| cwd.starts_with(&root) && root.starts_with(&git_root))
}

/// Resolve the registered checkout represented by this cwd/root pair in a
/// registry the caller already loaded for `roots.global_root`.
///
/// Cwd is authoritative when several checkouts deliberately share one
/// explicit root. The historical orbit-dir fallback remains for ordinary and
/// linked-worktree roots, where the shared root is still a checkout identity.
pub(crate) fn select_workspace_for_cwd_and_roots(
    cwd: &Path,
    roots: &OrbitRuntimeRoots,
    registry: &WorkspaceRegistry,
) -> Result<Option<ResolvedWorkspaceSelection>, OrbitError> {
    let shared = canonical_or_original(&roots.shared_root);

    if let Some(checkout) = workspace_registry::find_checkout_by_path(registry, cwd)
        && canonical_or_original(&checkout.orbit_dir) == shared
        && let Some(workspace) =
            workspace_registry::find_workspace_by_id(registry, &checkout.workspace_id)
    {
        return Ok(Some(ResolvedWorkspaceSelection {
            workspace: workspace.clone(),
            checkout: checkout.clone(),
            local_root: roots.local_root.clone(),
        }));
    }

    // An explicit --root pins global_root to shared_root. In that mode an
    // orbit-dir-only fallback would silently select the first unrelated
    // checkout registered under the shared data directory.
    if canonical_or_original(&roots.global_root) == shared {
        return Ok(None);
    }

    Ok(
        registered_checkout_for_shared_root(registry, &shared).map(|(workspace, checkout)| {
            ResolvedWorkspaceSelection {
                workspace: workspace.clone(),
                checkout: checkout.clone(),
                local_root: roots.local_root.clone(),
            }
        }),
    )
}

pub(super) fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The registered checkout whose `.orbit` is the canonical `shared`, with its
/// logical workspace. One canonicalization per checkout serves both the
/// runtime binding and the replica-owner lookup of an open.
pub(super) fn registered_checkout_for_shared_root<'a>(
    registry: &'a WorkspaceRegistry,
    shared: &Path,
) -> Option<(&'a Workspace, &'a WorkspaceCheckout)> {
    workspace_registry::local_workspaces(registry)
        .find(|(_, checkout)| canonical_or_original(&checkout.orbit_dir) == shared)
}

/// Assemble registry-derived facts at the existing runtime composition
/// boundary, from the machine-identity classification the caller already read.
///
/// Only a complete identity names a machine: an uninitialized root leaves
/// automation unattributed rather than failing the open.
pub(super) fn attach_registry_context(
    runtime: OrbitRuntime,
    global_root: &Path,
    identity: &MachineIdentityState,
) -> OrbitRuntime {
    let machine_id = identity.id().map(ToOwned::to_owned);
    let runtime = attach_workspace_catalog(
        runtime.with_automation_machine_identity(machine_id),
        global_root,
    );
    crate::worker_coordination::attach(runtime)
}
