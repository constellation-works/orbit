//! Application composition over Registry's workspace catalog and Core's runtime seams.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::{
    HostLifetime, OrbitRuntimeRoots, ResolvedOrbitRoots, WorkspaceRootHint,
    WorkspaceRuntimeBinding, managed_workspace_selector_from_env,
};
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};
use serde_json::Value;

use orbit_registry::{
    HostIdentityState, inspect_host_identity, load_host_identity, workspace_registry,
};

use crate::workspace_catalog::attach as attach_workspace_catalog;

/// Registered workspace metadata keeps the logical catalog ID distinct from the
/// task/runtime ID stored in `.orbit/config.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkspaceBinding {
    pub logical_workspace_id: String,
    pub runtime: WorkspaceRuntimeBinding,
    pub role: Option<WorkspaceCheckoutRole>,
    pub owner_machine_id: Option<String>,
}

/// One server-local workspace selected from the registry, before Core opens a
/// runtime for it.
#[derive(Debug, Clone)]
pub struct ResolvedWorkspaceSelection {
    pub workspace: Workspace,
    pub checkout: WorkspaceCheckout,
}

/// Build Core's authoritative runtime binding for a registered checkout.
/// The runtime ID deliberately comes from config.yaml rather than the logical
/// registry record because legacy installations may validly differ (L-0098).
pub fn workspace_runtime_binding(
    workspace: &Workspace,
    checkout: &WorkspaceCheckout,
) -> Result<WorkspaceRuntimeBinding, OrbitError> {
    orbit_core::runtime::workspace_runtime_binding(workspace, checkout)
}

pub fn resolved_workspace_binding(
    workspace: &Workspace,
    checkout: &WorkspaceCheckout,
) -> Result<ResolvedWorkspaceBinding, OrbitError> {
    Ok(ResolvedWorkspaceBinding {
        logical_workspace_id: workspace.id.clone(),
        runtime: workspace_runtime_binding(workspace, checkout)?,
        role: checkout.role,
        owner_machine_id: workspace.owner_machine_id.clone(),
    })
}

/// Registry-aware runtime factory. Registered checkouts carry an explicit
/// Core workspace binding.
pub struct RegisteredRuntimeFactory;

impl RegisteredRuntimeFactory {
    pub fn resolve_roots_for_cwd(
        cwd: &Path,
        root_override: Option<&Path>,
    ) -> Result<OrbitRuntimeRoots, OrbitError> {
        let hint = workspace_root_hint(cwd);
        OrbitRuntime::resolve_roots_for_cwd_with_hint(cwd, root_override, hint.as_ref())
    }

    pub fn resolve_bootstrap_roots_for_cwd(
        cwd: &Path,
        root_override: Option<&Path>,
    ) -> Result<OrbitRuntimeRoots, OrbitError> {
        let hint = bootstrap_workspace_root_hint(cwd);
        OrbitRuntime::resolve_bootstrap_roots_for_cwd_with_hint(cwd, root_override, hint.as_ref())
    }

    pub fn try_resolve_initialized_roots(
        cwd: &Path,
        root_override: Option<&Path>,
    ) -> Result<Option<ResolvedOrbitRoots>, OrbitError> {
        let hint = workspace_root_hint(cwd);
        orbit_core::runtime::try_resolve_initialized_roots_with_hint(
            cwd,
            root_override,
            hint.as_ref(),
        )
    }

    pub fn initialize_with_root_override(
        root_override: Option<&Path>,
    ) -> Result<OrbitRuntime, OrbitError> {
        Self::initialize_with_overrides(root_override, None)
    }

    /// Bootstrap a CLI runtime from `--root` and/or `--workspace`.
    ///
    /// `--workspace` is the workspace selector (name, `ws_*` id, or absolute
    /// checkout path). `--root` stays a data-directory override and is never
    /// overloaded as a selector. When `--root` is omitted, omitting
    /// `--workspace` uses the trusted managed `ORBIT_WORKSPACE` envelope when
    /// present, otherwise the cwd walk. An explicit `--root` instead owns the
    /// complete registry and workspace-resolution context, so a managed
    /// selector inherited from the parent cannot escape that root. An unknown
    /// or mismatched explicit selector fails closed and does not fall back to
    /// cwd.
    pub fn initialize_with_overrides(
        root_override: Option<&Path>,
        workspace_selector: Option<&str>,
    ) -> Result<OrbitRuntime, OrbitError> {
        Self::initialize_with_overrides_mode(root_override, workspace_selector, false)
    }

    /// Construct a workspace runtime for an observation command without the
    /// usual stale-run reconciliation performed during normal runtime open.
    pub fn initialize_read_only_with_overrides(
        root_override: Option<&Path>,
        workspace_selector: Option<&str>,
    ) -> Result<OrbitRuntime, OrbitError> {
        Self::initialize_with_overrides_mode(root_override, workspace_selector, true)
    }

    fn initialize_with_overrides_mode(
        root_override: Option<&Path>,
        workspace_selector: Option<&str>,
        read_only: bool,
    ) -> Result<OrbitRuntime, OrbitError> {
        let explicit_selector = workspace_selector
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let selector = explicit_selector.or_else(|| {
            if root_override.is_some() {
                None
            } else {
                managed_workspace_selector_from_env()
            }
        });
        let Some(selector) = selector else {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let roots = Self::resolve_roots_for_cwd(&cwd, root_override)?;
            sync_task_prefix(&roots.global_root)?;
            let selection = select_workspace_for_cwd_and_roots(&cwd, &roots)?;
            let binding = selection
                .as_ref()
                .map(|selection| {
                    workspace_runtime_binding(&selection.workspace, &selection.checkout)
                })
                .transpose()?;
            let replica_owner = selection
                .as_ref()
                .and_then(|selection| replica_owner_for_checkout(&selection.checkout));
            let global_root = roots.global_root.clone();
            let runtime = if read_only {
                OrbitRuntime::initialize_from_resolved_roots_read_only(roots, binding)
            } else {
                OrbitRuntime::initialize_from_resolved_roots(roots, binding)
            };
            return runtime.map(|runtime| {
                attach_registry_context(
                    runtime.with_coordination_write_owner(replica_owner),
                    &global_root,
                )
            });
        };

        let global_root = global_root_for(root_override)?;
        let selected = Self::resolve_workspace_selector(&global_root, &selector)?;
        if read_only {
            Self::open_registered_checkout_read_only(
                &global_root,
                &selected.workspace,
                &selected.checkout,
            )
        } else {
            Self::open_registered_checkout(&global_root, &selected.workspace, &selected.checkout)
        }
    }

    /// Resolve a workspace selector against this machine's registry.
    ///
    /// This is the shared server/CLI bootstrap seam. It performs correctness
    /// checks needed to construct a runtime, but makes no transport,
    /// authorization, or cross-machine routing decision.
    ///
    /// Non-active workspaces (such as a workspace whose checkout directory was
    /// deleted without teardown) cannot be bound by any CLI verb, including
    /// read-only inspection commands (`task list`, `task show`, `workspace show`,
    /// `doctor`), even if the Orbit root is still readable. The command fails
    /// closed naming the status and recorded checkout path.
    pub fn resolve_workspace_selector(
        global_root: &Path,
        selector: &str,
    ) -> Result<ResolvedWorkspaceSelection, OrbitError> {
        let registry_path = workspace_registry::registry_path_for(global_root);
        let mut registry = workspace_registry::load_registry_from(&registry_path)?;
        if workspace_registry::validate_workspaces(&mut registry) {
            let _ = workspace_registry::save_registry_to(&registry, &registry_path);
        }
        let (workspace, checkout) = resolve_cli_workspace_binding(&registry, selector)?;
        if workspace.status != WorkspaceStatus::Active {
            return Err(inactive_cli_workspace(workspace, checkout));
        }
        Ok(ResolvedWorkspaceSelection {
            workspace: workspace.clone(),
            checkout: checkout.clone(),
        })
    }

    pub fn open_resolved_roots(roots: OrbitRuntimeRoots) -> Result<OrbitRuntime, OrbitError> {
        sync_task_prefix(&roots.global_root)?;
        let binding = binding_for_roots(&roots)?;
        let replica_owner = replica_owner_for_roots(&roots)?;
        let runtime = match binding {
            Some(binding) => OrbitRuntime::from_resolved_roots_with_binding(
                &roots.global_root,
                &roots.shared_root,
                &roots.local_root,
                binding,
            ),
            None => OrbitRuntime::from_resolved_roots(
                &roots.global_root,
                &roots.shared_root,
                &roots.local_root,
            ),
        }?;
        Ok(attach_registry_context(
            runtime.with_coordination_write_owner(replica_owner),
            &roots.global_root,
        ))
    }

    pub fn open_registered_checkout(
        global_root: &Path,
        workspace: &Workspace,
        checkout: &WorkspaceCheckout,
    ) -> Result<OrbitRuntime, OrbitError> {
        Self::open_registered_checkout_for(
            global_root,
            workspace,
            checkout,
            HostLifetime::ShortLived,
        )
    }

    pub fn open_registered_checkout_for(
        global_root: &Path,
        workspace: &Workspace,
        checkout: &WorkspaceCheckout,
        host_lifetime: HostLifetime,
    ) -> Result<OrbitRuntime, OrbitError> {
        sync_task_prefix(global_root)?;
        let binding = workspace_runtime_binding(workspace, checkout)?;
        OrbitRuntime::from_roots_with_binding_for(
            global_root,
            &checkout.orbit_dir,
            binding,
            host_lifetime,
        )
        .map(|runtime| {
            attach_registry_context(
                runtime.with_coordination_write_owner(replica_owner_for_checkout(checkout)),
                global_root,
            )
        })
    }

    fn open_registered_checkout_read_only(
        global_root: &Path,
        workspace: &Workspace,
        checkout: &WorkspaceCheckout,
    ) -> Result<OrbitRuntime, OrbitError> {
        sync_task_prefix(global_root)?;
        let binding = workspace_runtime_binding(workspace, checkout)?;
        OrbitRuntime::from_resolved_roots_read_only_with_binding(
            global_root,
            &checkout.orbit_dir,
            &checkout.orbit_dir,
            binding,
        )
        .map(|runtime| {
            attach_registry_context(
                runtime.with_coordination_write_owner(replica_owner_for_checkout(checkout)),
                global_root,
            )
        })
    }

    pub fn open_resolved_checkout(
        global_root: &Path,
        shared_root: &Path,
        local_root: &Path,
        binding: WorkspaceRuntimeBinding,
    ) -> Result<OrbitRuntime, OrbitError> {
        Self::open_resolved_checkout_for(
            global_root,
            shared_root,
            local_root,
            binding,
            HostLifetime::ShortLived,
        )
    }

    pub fn open_resolved_checkout_for(
        global_root: &Path,
        shared_root: &Path,
        local_root: &Path,
        binding: WorkspaceRuntimeBinding,
        host_lifetime: HostLifetime,
    ) -> Result<OrbitRuntime, OrbitError> {
        sync_task_prefix(global_root)?;
        OrbitRuntime::from_resolved_roots_with_binding_for(
            global_root,
            shared_root,
            local_root,
            binding,
            host_lifetime,
        )
        .map(|runtime| attach_registry_context(runtime, global_root))
    }

    /// Bind a CLI `orbit tool run` invocation to the workspace named in `input`.
    ///
    /// This is the single local CLI resolver above the tools. A non-empty
    /// `workspace` either rebinds the runtime to that registered checkout or
    /// fails closed naming the selector. MCP resolves selectors independently
    /// on the accepting server and never falls back to process cwd.
    pub fn bind_cli_tool_workspace(
        runtime: &OrbitRuntime,
        input: &mut Value,
    ) -> Result<Option<OrbitRuntime>, OrbitError> {
        let Some(selector) = cli_workspace_selector(input)? else {
            return Ok(None);
        };
        let global_root = runtime.global_root();
        let registry_path = workspace_registry::registry_path_for(&global_root);
        let mut registry = workspace_registry::load_registry_from(&registry_path)?;
        if workspace_registry::validate_workspaces(&mut registry) {
            let _ = workspace_registry::save_registry_to(&registry, &registry_path);
        }
        match resolve_cli_workspace_target(&registry, runtime, &selector)? {
            CliWorkspaceTarget::CurrentRuntime => Ok(None),
            CliWorkspaceTarget::Checkout {
                workspace,
                checkout,
                rewrite_to_repo_root,
            } => {
                if workspace.status != WorkspaceStatus::Active {
                    return Err(inactive_cli_workspace(workspace, checkout));
                }
                if rewrite_to_repo_root {
                    set_input_workspace(input, &checkout.repo_root)?;
                }
                if same_cli_checkout(runtime, checkout) {
                    return Ok(None);
                }
                Self::open_registered_checkout(&global_root, workspace, checkout).map(Some)
            }
        }
    }
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

enum CliWorkspaceTarget<'a> {
    CurrentRuntime,
    Checkout {
        workspace: &'a Workspace,
        checkout: &'a WorkspaceCheckout,
        rewrite_to_repo_root: bool,
    },
}

fn cli_workspace_selector(input: &Value) -> Result<Option<String>, OrbitError> {
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

fn resolve_cli_workspace_target<'a>(
    registry: &'a WorkspaceRegistry,
    runtime: &OrbitRuntime,
    selector: &str,
) -> Result<CliWorkspaceTarget<'a>, OrbitError> {
    if selector_looks_like_path(selector) {
        return resolve_cli_workspace_path(registry, Some(runtime), selector);
    }
    let (workspace, checkout) = resolve_named_cli_checkout(registry, selector)?;
    Ok(CliWorkspaceTarget::Checkout {
        workspace,
        checkout,
        rewrite_to_repo_root: true,
    })
}

fn resolve_cli_workspace_binding<'a>(
    registry: &'a WorkspaceRegistry,
    selector: &str,
) -> Result<(&'a Workspace, &'a WorkspaceCheckout), OrbitError> {
    match if selector_looks_like_path(selector) {
        resolve_cli_workspace_path(registry, None, selector)?
    } else {
        let (workspace, checkout) = resolve_named_cli_checkout(registry, selector)?;
        CliWorkspaceTarget::Checkout {
            workspace,
            checkout,
            rewrite_to_repo_root: true,
        }
    } {
        CliWorkspaceTarget::Checkout {
            workspace,
            checkout,
            ..
        } => Ok((workspace, checkout)),
        CliWorkspaceTarget::CurrentRuntime => Err(unsupported_cli_workspace(selector)),
    }
}

fn resolve_named_cli_checkout<'a>(
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

fn resolve_cli_workspace_path<'a>(
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
        });
    }
    Err(unsupported_cli_workspace(selector))
}

fn find_checkout_for_canonical_path<'a>(
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

fn find_checkout_for_raw_path<'a>(
    registry: &'a WorkspaceRegistry,
    candidate: &Path,
) -> Option<&'a WorkspaceCheckout> {
    let normalized = normalize_path(candidate);
    find_checkout_for_canonical_path(registry, &normalized)
        .or_else(|| workspace_registry::find_checkout_by_path(registry, &normalized))
}

fn find_checkout_for_git_common_dir<'a>(
    registry: &'a WorkspaceRegistry,
    selected: &Path,
) -> Option<&'a WorkspaceCheckout> {
    let selected_common = git_common_dir(selected)?;
    let mut matches = registry.checkouts.iter().filter(|checkout| {
        git_common_dir(&checkout.repo_root).is_some_and(|common| common == selected_common)
    });
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn git_common_dir(path: &Path) -> Option<PathBuf> {
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

fn same_cli_checkout(runtime: &OrbitRuntime, checkout: &WorkspaceCheckout) -> bool {
    canonical_path(&runtime.paths().repo_root) == canonical_path(&checkout.repo_root)
}

fn path_is_inside(parent: &Path, child: &Path) -> bool {
    child.starts_with(canonical_path(parent))
}

fn canonical_path(path: &Path) -> PathBuf {
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

fn set_input_workspace(input: &mut Value, repo_root: &Path) -> Result<(), OrbitError> {
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

fn normalize_path(path: &Path) -> PathBuf {
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

fn unsupported_cli_workspace(selector: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "unknown workspace selector '{selector}'; pass a registered workspace name, a logical workspace ID, or an absolute local checkout path"
    ))
}

fn inactive_cli_workspace(workspace: &Workspace, checkout: &WorkspaceCheckout) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "workspace '{}' ({}) is {} on this machine; recorded checkout path: {}",
        workspace.name,
        workspace.id,
        workspace.status,
        checkout.repo_root.display(),
    ))
}

/// Project the host-owned task namespace into the neutral task allocator.
/// Custom/legacy roots without host.toml retain the historical ORB default;
/// once an identity exists, malformed or conflicting state fails closed.
pub(crate) fn sync_task_prefix(global_root: &Path) -> Result<(), OrbitError> {
    let identity = match inspect_host_identity(global_root)? {
        HostIdentityState::Present(identity) => identity,
        HostIdentityState::Absent => return Ok(()),
        // Keep legacy files on the established migration-required path while
        // using Registry's validated classifier for every host.toml access.
        HostIdentityState::Legacy { .. } => load_host_identity(global_root)?,
    };

    let registry = TaskRegistryStore::open(&task_registry_path(global_root))?;
    registry.set_task_prefix(&identity.task_prefix)
}

fn replica_owner_for_checkout(checkout: &WorkspaceCheckout) -> Option<String> {
    (checkout.role == Some(WorkspaceCheckoutRole::Replica))
        .then(|| checkout.owner_machine_id.clone())
        .flatten()
}

fn replica_owner_for_roots(roots: &OrbitRuntimeRoots) -> Result<Option<String>, OrbitError> {
    let registry_path = workspace_registry::registry_path_for(&roots.global_root);
    let registry = workspace_registry::load_registry_from(&registry_path)?;
    let shared =
        std::fs::canonicalize(&roots.shared_root).unwrap_or_else(|_| roots.shared_root.clone());
    Ok(registry.checkouts.iter().find_map(|checkout| {
        let registered = std::fs::canonicalize(&checkout.orbit_dir)
            .unwrap_or_else(|_| checkout.orbit_dir.clone());
        (registered == shared)
            .then(|| replica_owner_for_checkout(checkout))
            .flatten()
    }))
}

fn workspace_root_hint(cwd: &Path) -> Option<WorkspaceRootHint> {
    let registry = workspace_registry::load_registry().ok()?;
    let checkout = workspace_registry::find_checkout_by_path(&registry, cwd)?;
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
fn bootstrap_workspace_root_hint(cwd: &Path) -> Option<WorkspaceRootHint> {
    let registry = workspace_registry::load_registry().ok()?;
    let checkout = workspace_registry::find_checkout_by_path(&registry, cwd)?;
    if checkout_crosses_nested_git_boundary(checkout, cwd) {
        return None;
    }
    Some(WorkspaceRootHint {
        orbit_dir: checkout.orbit_dir.clone(),
    })
}

fn checkout_crosses_nested_git_boundary(checkout: &WorkspaceCheckout, cwd: &Path) -> bool {
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

fn binding_for_roots(
    roots: &OrbitRuntimeRoots,
) -> Result<Option<WorkspaceRuntimeBinding>, OrbitError> {
    let registry_path = workspace_registry::registry_path_for(&roots.global_root);
    let registry = workspace_registry::load_registry_from(&registry_path)?;
    binding_for_registry_roots(&registry, &roots.shared_root)
}

/// Resolve the registered checkout represented by this cwd/root pair.
///
/// Cwd is authoritative when several checkouts deliberately share one
/// explicit root. The historical orbit-dir fallback remains for ordinary and
/// linked-worktree roots, where the shared root is still a checkout identity.
pub(crate) fn select_workspace_for_cwd_and_roots(
    cwd: &Path,
    roots: &OrbitRuntimeRoots,
) -> Result<Option<ResolvedWorkspaceSelection>, OrbitError> {
    let registry_path = workspace_registry::registry_path_for(&roots.global_root);
    let registry = workspace_registry::load_registry_from(&registry_path)?;
    let shared = canonical_or_original(&roots.shared_root);

    if let Some(checkout) = workspace_registry::find_checkout_by_path(&registry, cwd)
        && canonical_or_original(&checkout.orbit_dir) == shared
        && let Some(workspace) =
            workspace_registry::find_workspace_by_id(&registry, &checkout.workspace_id)
    {
        return Ok(Some(ResolvedWorkspaceSelection {
            workspace: workspace.clone(),
            checkout: checkout.clone(),
        }));
    }

    // An explicit --root pins global_root to shared_root. In that mode an
    // orbit-dir-only fallback would silently select the first unrelated
    // checkout registered under the shared data directory.
    if canonical_or_original(&roots.global_root) == shared {
        return Ok(None);
    }

    for (workspace, checkout) in workspace_registry::local_workspaces(&registry) {
        if canonical_or_original(&checkout.orbit_dir) == shared {
            return Ok(Some(ResolvedWorkspaceSelection {
                workspace: workspace.clone(),
                checkout: checkout.clone(),
            }));
        }
    }
    Ok(None)
}

fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn binding_for_registry_roots(
    registry: &WorkspaceRegistry,
    shared_root: &Path,
) -> Result<Option<WorkspaceRuntimeBinding>, OrbitError> {
    let shared = std::fs::canonicalize(shared_root).unwrap_or_else(|_| shared_root.to_path_buf());
    for (workspace, checkout) in workspace_registry::local_workspaces(registry) {
        let registered = std::fs::canonicalize(&checkout.orbit_dir)
            .unwrap_or_else(|_| checkout.orbit_dir.clone());
        if registered == shared {
            return workspace_runtime_binding(workspace, checkout).map(Some);
        }
    }
    Ok(None)
}

/// Assemble registry-derived facts at the existing runtime composition boundary.
fn attach_registry_context(runtime: OrbitRuntime, global_root: &Path) -> OrbitRuntime {
    let machine_id = load_host_identity(global_root)
        .ok()
        .map(|identity| identity.machine_id);
    attach_workspace_catalog(
        runtime.with_automation_machine_identity(machine_id),
        global_root,
    )
}
