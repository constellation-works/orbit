//! Application composition over Registry's workspace catalog and Core's runtime seams.

#[cfg(test)]
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::{
    HostLifetime, OrbitRuntimeRoots, ResolvedOrbitRoots, WorkspaceRootHint,
    WorkspaceRuntimeBinding, managed_workspace_selector_from_env,
};
use orbit_store::maintenance::task_registry::{
    TaskRegistryStore, task_registry_path, workspace_config_path,
};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};
use serde_json::Value;

use orbit_registry::{
    HOST_TOML_FILE, HostIdentityState, inspect_host_identity, load_host_identity,
    workspace_registry,
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
    /// Definition/artifact root for the specifically selected checkout.
    ///
    /// A logical name or ID selects the registered primary checkout. An
    /// explicit linked-worktree path keeps shared stores on that registered
    /// checkout while using the linked checkout's `.orbit` for Git-versioned
    /// local definitions.
    pub local_root: PathBuf,
}

/// Freshness stamp for the files [`RegisteredRuntimeFactory`] reads while it
/// composes a runtime for one registered checkout.
///
/// Size and modification time, not content: the point is for a long-lived host
/// that keeps a built runtime to notice an edit without re-opening anything. A
/// caller that also compares the registry records it resolved (as the MCP
/// server does) covers same-size edits to those records; the stamp covers the
/// remaining composition inputs — the rest of the registry file, the host
/// identity behind the task-prefix projection and machine identity, the
/// checkout's own `config.yaml` task binding, and both `config.toml` layers
/// the runtime settings (crews, default crew, execution policy) are resolved
/// from at open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredRuntimeStamp {
    registry: FileStamp,
    host_identity: FileStamp,
    workspace_binding: FileStamp,
    global_config: FileStamp,
    workspace_config: FileStamp,
}

impl RegisteredRuntimeStamp {
    /// Stamp the composition inputs for `checkout`. An absent or unreadable
    /// file stamps as nothing, so a file that later appears or disappears is
    /// itself a change.
    ///
    /// The `config.toml` paths mirror `ConfigRoots::new(global_root,
    /// shared_root)` in Core's composition, where a registered checkout's
    /// shared root is its own `.orbit` directory.
    pub fn read(global_root: &Path, checkout: &WorkspaceCheckout) -> Self {
        Self {
            registry: FileStamp::read(&workspace_registry::registry_path_for(global_root)),
            host_identity: FileStamp::read(&global_root.join(HOST_TOML_FILE)),
            workspace_binding: FileStamp::read(&workspace_config_path(&checkout.orbit_dir)),
            global_config: FileStamp::read(&global_root.join(CONFIG_TOML_FILE)),
            workspace_config: FileStamp::read(&checkout.orbit_dir.join(CONFIG_TOML_FILE)),
        }
    }
}

/// The layered runtime configuration file, read from both `global_root` and
/// the checkout's `.orbit` directory (`orbit_config::ConfigRoots`).
const CONFIG_TOML_FILE: &str = "config.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp(Option<(SystemTime, u64)>);

impl FileStamp {
    fn read(path: &Path) -> Self {
        Self(
            std::fs::metadata(path)
                .and_then(|metadata| Ok((metadata.modified()?, metadata.len())))
                .ok(),
        )
    }
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

/// Admit retry attempts for fifteen seconds. An attempt already in SQLite may
/// consume its five-second busy timeout, so wall-clock bootstrap is bounded to
/// twenty seconds. The parent observer has no shorter kill deadline and keeps
/// supervising an unclaimed pending child throughout this recovery window.
const PIPELINE_WORKER_BOOTSTRAP_RETRY_DEADLINE: Duration = Duration::from_secs(15);
const PIPELINE_WORKER_BOOTSTRAP_RETRY_INTERVAL: Duration = Duration::from_millis(100);

impl RegisteredRuntimeFactory {
    /// Bootstrap the hidden detached worker with lock-only bounded recovery.
    pub fn initialize_pipeline_worker_with_overrides(
        root_override: Option<&Path>,
        workspace_selector: Option<&str>,
    ) -> Result<OrbitRuntime, OrbitError> {
        retry_pipeline_worker_bootstrap(
            || Self::initialize_with_overrides(root_override, workspace_selector),
            PIPELINE_WORKER_BOOTSTRAP_RETRY_DEADLINE,
            PIPELINE_WORKER_BOOTSTRAP_RETRY_INTERVAL,
        )
    }

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
    ///
    /// When cwd is a Git-linked worktree of the selected workspace, Git-versioned
    /// local definitions use that worktree's `.orbit` while shared task/runtime
    /// state stays on the registered primary. A logical selector from a cwd that
    /// is not such a worktree still opens the primary as `local_root`.
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
            // Without `--root`, the roots resolve against `resolve_global_root`,
            // so one read of that registry serves both the catalog hint and
            // the selection below. A failed read keeps the hint absent, as
            // before; the error surfaces from the re-read once roots resolve.
            let preloaded = match root_override {
                Some(_) => None,
                None => orbit_core::runtime::resolve_global_root()
                    .and_then(|global_root| RuntimeOpenInputs::read(&global_root))
                    .ok(),
            };
            let hint = preloaded
                .as_ref()
                .and_then(|inputs| checkout_root_hint(&inputs.registry, &cwd));
            let roots =
                OrbitRuntime::resolve_roots_for_cwd_with_hint(&cwd, root_override, hint.as_ref())?;
            let inputs = match preloaded {
                Some(inputs) if inputs.global_root == roots.global_root => inputs,
                _ => RuntimeOpenInputs::read(&roots.global_root)?,
            };
            if !read_only {
                sync_task_prefix_for_identity(&roots.global_root, &inputs.identity)?;
            }
            let selection = select_workspace_for_cwd_and_roots(&cwd, &roots, &inputs.registry)?;
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
                    &inputs.identity,
                )
            });
        };

        let global_root = global_root_for(root_override)?;
        let identity = inspect_host_identity(&global_root)?;
        let registry = load_registry_for_selector_resolution(
            &workspace_registry::registry_path_for(&global_root),
            &identity,
        )?;
        let selected = Self::resolve_selector_in(&registry, &selector)?;
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let local_root = local_root_for_runtime_open(&selected, &cwd, read_only);
        if read_only {
            Self::open_registered_checkout_read_only_with_identity(
                &global_root,
                &selected.workspace,
                &selected.checkout,
                &local_root,
                &identity,
            )
        } else {
            Self::open_registered_checkout_with_identity_and_local_root(
                &global_root,
                &selected.workspace,
                &selected.checkout,
                &local_root,
                HostLifetime::ShortLived,
                &identity,
            )
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
        let registry = Self::load_registry_for_selectors(global_root)?;
        Self::resolve_selector_in(&registry, selector)
    }

    /// The registry read [`Self::resolve_workspace_selector`] performs, exposed
    /// so a caller resolving several selectors at once reads `workspaces.json`
    /// once rather than once per selector [DANI-10365].
    pub(crate) fn load_registry_for_selectors(
        global_root: &Path,
    ) -> Result<WorkspaceRegistry, OrbitError> {
        load_registry_for_selector_resolution(
            &workspace_registry::registry_path_for(global_root),
            &inspect_host_identity(global_root)?,
        )
    }

    /// [`Self::resolve_workspace_selector`] against a registry already loaded.
    pub(crate) fn resolve_selector_in(
        registry: &WorkspaceRegistry,
        selector: &str,
    ) -> Result<ResolvedWorkspaceSelection, OrbitError> {
        let (workspace, checkout, local_root) = resolve_cli_workspace_binding(registry, selector)?;
        if workspace.status != WorkspaceStatus::Active {
            return Err(inactive_cli_workspace(workspace, checkout));
        }
        Ok(ResolvedWorkspaceSelection {
            workspace: workspace.clone(),
            checkout: checkout.clone(),
            local_root,
        })
    }

    pub fn open_resolved_roots(roots: OrbitRuntimeRoots) -> Result<OrbitRuntime, OrbitError> {
        let inputs = RuntimeOpenInputs::read(&roots.global_root)?;
        sync_task_prefix_for_identity(&roots.global_root, &inputs.identity)?;
        let registered = registered_checkout_for_shared_root(
            &inputs.registry,
            &canonical_or_original(&roots.shared_root),
        );
        let binding = registered
            .map(|(workspace, checkout)| workspace_runtime_binding(workspace, checkout))
            .transpose()?;
        let replica_owner =
            registered.and_then(|(_, checkout)| replica_owner_for_checkout(checkout));
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
            &inputs.identity,
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
        // One host-identity read serves both the task-prefix projection and the
        // automation machine identity; a long-lived host opens enough runtimes
        // for a second parse of the same `host.toml` to be pure overhead.
        let identity = inspect_host_identity(global_root)?;
        Self::open_registered_checkout_with_identity(
            global_root,
            workspace,
            checkout,
            host_lifetime,
            &identity,
        )
    }

    /// [`Self::open_registered_checkout_for`] from a `host.toml` classification
    /// the caller already read to resolve the selector [DANI-10371].
    fn open_registered_checkout_with_identity(
        global_root: &Path,
        workspace: &Workspace,
        checkout: &WorkspaceCheckout,
        host_lifetime: HostLifetime,
        identity: &HostIdentityState,
    ) -> Result<OrbitRuntime, OrbitError> {
        Self::open_registered_checkout_with_identity_and_local_root(
            global_root,
            workspace,
            checkout,
            &checkout.orbit_dir,
            host_lifetime,
            identity,
        )
    }

    /// Open a registered checkout with an explicit Git-versioned `local_root`.
    ///
    /// Shared stores stay on `checkout.orbit_dir`. Callers that stand in a
    /// Git-linked worktree of this checkout pass that worktree's `.orbit` so
    /// tracked definitions (auto-task YAML) write there instead of the
    /// registered primary [ORB-12665].
    fn open_registered_checkout_with_identity_and_local_root(
        global_root: &Path,
        workspace: &Workspace,
        checkout: &WorkspaceCheckout,
        local_root: &Path,
        host_lifetime: HostLifetime,
        identity: &HostIdentityState,
    ) -> Result<OrbitRuntime, OrbitError> {
        sync_task_prefix_for_identity(global_root, identity)?;
        let binding = workspace_runtime_binding(workspace, checkout)?;
        OrbitRuntime::from_resolved_roots_with_binding_for(
            global_root,
            &checkout.orbit_dir,
            local_root,
            binding,
            host_lifetime,
        )
        .map(|runtime| {
            attach_registry_context(
                runtime.with_coordination_write_owner(replica_owner_for_checkout(checkout)),
                global_root,
                identity,
            )
        })
    }

    /// Open a registered checkout for an observation command without stale-run
    /// reconciliation or other normal runtime-open writes.
    pub fn open_registered_checkout_read_only(
        global_root: &Path,
        workspace: &Workspace,
        checkout: &WorkspaceCheckout,
    ) -> Result<OrbitRuntime, OrbitError> {
        let identity = inspect_host_identity(global_root)?;
        Self::open_registered_checkout_read_only_with_identity(
            global_root,
            workspace,
            checkout,
            &checkout.orbit_dir,
            &identity,
        )
    }

    fn open_registered_checkout_read_only_with_identity(
        global_root: &Path,
        workspace: &Workspace,
        checkout: &WorkspaceCheckout,
        local_root: &Path,
        identity: &HostIdentityState,
    ) -> Result<OrbitRuntime, OrbitError> {
        let binding = workspace_runtime_binding(workspace, checkout)?;
        OrbitRuntime::from_resolved_roots_read_only_with_binding(
            global_root,
            &checkout.orbit_dir,
            local_root,
            binding,
        )
        .map(|runtime| {
            attach_registry_context(
                runtime.with_coordination_write_owner(replica_owner_for_checkout(checkout)),
                global_root,
                identity,
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
        let identity = inspect_host_identity(global_root)?;
        sync_task_prefix_for_identity(global_root, &identity)?;
        OrbitRuntime::from_resolved_roots_with_binding_for(
            global_root,
            shared_root,
            local_root,
            binding,
            host_lifetime,
        )
        .map(|runtime| attach_registry_context(runtime, global_root, &identity))
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
        let identity = inspect_host_identity(&global_root)?;
        let registry = load_registry_for_selector_resolution(
            &workspace_registry::registry_path_for(&global_root),
            &identity,
        )?;
        match resolve_cli_workspace_target(&registry, runtime, &selector)? {
            CliWorkspaceTarget::CurrentRuntime => Ok(None),
            CliWorkspaceTarget::Checkout {
                workspace,
                checkout,
                rewrite_to_repo_root,
                ..
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
                Self::open_registered_checkout_with_identity(
                    &global_root,
                    workspace,
                    checkout,
                    HostLifetime::ShortLived,
                    &identity,
                )
                .map(Some)
            }
        }
    }
}

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
fn load_registry_for_selector_resolution(
    registry_path: &Path,
    identity: &HostIdentityState,
) -> Result<WorkspaceRegistry, OrbitError> {
    let loaded =
        workspace_registry::load_registry_from_read_only_with_host(registry_path, identity)?;
    let mut registry = loaded.registry;
    let validation_required = workspace_registry::validate_workspaces(&mut registry);
    if !loaded.migration_required && !validation_required {
        return Ok(registry);
    }

    workspace_registry::with_registry_lock(registry_path, || {
        let mut registry =
            workspace_registry::load_registry_from_with_host(registry_path, identity)?;
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

enum CliWorkspaceTarget<'a> {
    CurrentRuntime,
    Checkout {
        workspace: &'a Workspace,
        checkout: &'a WorkspaceCheckout,
        rewrite_to_repo_root: bool,
        local_root: PathBuf,
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
        local_root: checkout.orbit_dir.clone(),
    })
}

fn resolve_cli_workspace_binding<'a>(
    registry: &'a WorkspaceRegistry,
    selector: &str,
) -> Result<(&'a Workspace, &'a WorkspaceCheckout, PathBuf), OrbitError> {
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
fn recorded_git_common_dir(checkout: &WorkspaceCheckout) -> Option<PathBuf> {
    let git_dir = checkout.orbit_dir.parent()?.join(".git");
    git_dir.is_dir().then(|| canonical_path(&git_dir))
}

fn git_common_dir(path: &Path) -> Option<PathBuf> {
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
fn local_root_for_selected_path(checkout: &WorkspaceCheckout, selected: &Path) -> PathBuf {
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
fn local_root_for_runtime_open(
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
fn linked_worktree_local_root(checkout: &WorkspaceCheckout, cwd: &Path) -> Option<PathBuf> {
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
fn git_workdir_root(path: &Path) -> Option<PathBuf> {
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

/// The two files every runtime open reads before it dispatches anything:
/// `host.toml`, which classifies this machine for the task-prefix projection,
/// the automation identity, and registry validation; and `workspaces.json`,
/// which selects the checkout. Agents shell out to `orbit` hundreds of times
/// per run, so each is read once and threaded through the open rather than
/// re-read by every step that needs it [DANI-10371].
struct RuntimeOpenInputs {
    global_root: PathBuf,
    identity: HostIdentityState,
    registry: WorkspaceRegistry,
}

impl RuntimeOpenInputs {
    fn read(global_root: &Path) -> Result<Self, OrbitError> {
        let identity = inspect_host_identity(global_root)?;
        let registry = workspace_registry::load_registry_from_with_host(
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

/// Project the host-owned task namespace into the neutral task allocator.
/// Custom/legacy roots without host.toml retain the historical ORB default;
/// once an identity exists, malformed or conflicting state fails closed.
pub(crate) fn sync_task_prefix(global_root: &Path) -> Result<(), OrbitError> {
    sync_task_prefix_for_identity(global_root, &inspect_host_identity(global_root)?)
}

/// The same projection for a caller that already classified `host.toml`.
fn sync_task_prefix_for_identity(
    global_root: &Path,
    identity: &HostIdentityState,
) -> Result<(), OrbitError> {
    let task_prefix = match identity {
        HostIdentityState::Present(identity) => identity.task_prefix.clone(),
        HostIdentityState::Absent => return Ok(()),
        // Keep legacy files on the established migration-required path while
        // using Registry's validated classifier for every host.toml access.
        HostIdentityState::Legacy { .. } => load_host_identity(global_root)?.task_prefix,
    };

    let registry = TaskRegistryStore::open(&task_registry_path(global_root))?;
    registry.set_task_prefix(&task_prefix)
}

fn replica_owner_for_checkout(checkout: &WorkspaceCheckout) -> Option<String> {
    (checkout.role == Some(WorkspaceCheckoutRole::Replica))
        .then(|| checkout.owner_machine_id.clone())
        .flatten()
}

fn workspace_root_hint(cwd: &Path) -> Option<WorkspaceRootHint> {
    let registry = workspace_registry::load_registry().ok()?;
    checkout_root_hint(&registry, cwd)
}

/// The catalog hint for `cwd` from a registry the caller already loaded.
fn checkout_root_hint(registry: &WorkspaceRegistry, cwd: &Path) -> Option<WorkspaceRootHint> {
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

fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The registered checkout whose `.orbit` is the canonical `shared`, with its
/// logical workspace. One canonicalization per checkout serves both the
/// runtime binding and the replica-owner lookup of an open.
fn registered_checkout_for_shared_root<'a>(
    registry: &'a WorkspaceRegistry,
    shared: &Path,
) -> Option<(&'a Workspace, &'a WorkspaceCheckout)> {
    workspace_registry::local_workspaces(registry)
        .find(|(_, checkout)| canonical_or_original(&checkout.orbit_dir) == shared)
}

/// Assemble registry-derived facts at the existing runtime composition
/// boundary, from the `host.toml` classification the caller already read.
///
/// Only a complete, current-schema identity names a machine: a legacy or
/// absent file leaves automation unattributed rather than failing the open.
fn attach_registry_context(
    runtime: OrbitRuntime,
    global_root: &Path,
    identity: &HostIdentityState,
) -> OrbitRuntime {
    let machine_id = match identity {
        HostIdentityState::Present(identity) => Some(identity.machine_id.clone()),
        HostIdentityState::Legacy { .. } | HostIdentityState::Absent => None,
    };
    let runtime = attach_workspace_catalog(
        runtime.with_automation_machine_identity(machine_id),
        global_root,
    );
    crate::worker_coordination::attach(runtime)
}
