use super::selection::*;
use super::*;

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
/// remaining composition inputs — the rest of the registry file, the
/// checkout's own `config.yaml` task binding, and both `config.toml` layers
/// the runtime settings (crews, default crew, execution policy, and this
/// machine's `[machine]` identity) are resolved from at open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredRuntimeStamp {
    registry: FileStamp,
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
        Self::initialize_with_overrides_mode(root_override, workspace_selector, false, false)
    }

    /// Construct a workspace runtime for an observation command without the
    /// usual stale-run reconciliation performed during normal runtime open.
    pub fn initialize_read_only_with_overrides(
        root_override: Option<&Path>,
        workspace_selector: Option<&str>,
    ) -> Result<OrbitRuntime, OrbitError> {
        Self::initialize_with_overrides_mode(root_override, workspace_selector, true, false)
    }

    /// Plugin inspection uses a registered checkout when one is selected by
    /// cwd or `--workspace`, otherwise it reads only the host's plugin state.
    /// An unregistered cwd must never become a workspace as a side effect.
    pub fn initialize_plugin_read_only_with_overrides(
        root_override: Option<&Path>,
        workspace_selector: Option<&str>,
    ) -> Result<OrbitRuntime, OrbitError> {
        Self::initialize_with_overrides_mode(root_override, workspace_selector, true, true)
    }

    fn initialize_with_overrides_mode(
        root_override: Option<&Path>,
        workspace_selector: Option<&str>,
        read_only: bool,
        host_fallback: bool,
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
            let mut roots =
                OrbitRuntime::resolve_roots_for_cwd_with_hint(&cwd, root_override, hint.as_ref())?;
            let inputs = match preloaded {
                Some(inputs) if inputs.global_root == roots.global_root => inputs,
                _ => RuntimeOpenInputs::read(&roots.global_root)?,
            };
            if !read_only {
                sync_task_prefix_for_identity(&roots.global_root, &inputs.identity)?;
            }
            let selection = select_workspace_for_cwd_and_roots(&cwd, &roots, &inputs.registry)?;
            if host_fallback && selection.is_none() {
                // A plugin inspection without a registered checkout has no
                // workspace pin file or workspace config to read. Keep every
                // runtime path at the host root, including the local root.
                roots.shared_root = roots.global_root.clone();
                roots.local_root = roots.global_root.clone();
            }
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
        let identity = inspect_machine_identity(&global_root)?;
        let registry = load_registry_for_selector_resolution(
            &workspace_registry::registry_path_for(&global_root),
            &identity,
        )?;
        let selected = Self::resolve_selector_in(&registry, &selector, identity.id())?;
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
        Self::resolve_selector_in(
            &registry,
            selector,
            inspect_machine_identity(global_root)?.id(),
        )
    }

    /// The registry read [`Self::resolve_workspace_selector`] performs, exposed
    /// so a caller resolving several selectors at once reads `workspaces.json`
    /// once rather than once per selector [DANI-10365].
    pub(crate) fn load_registry_for_selectors(
        global_root: &Path,
    ) -> Result<WorkspaceRegistry, OrbitError> {
        load_registry_for_selector_resolution(
            &workspace_registry::registry_path_for(global_root),
            &inspect_machine_identity(global_root)?,
        )
    }

    /// [`Self::resolve_workspace_selector`] against a registry already loaded.
    pub(crate) fn resolve_selector_in(
        registry: &WorkspaceRegistry,
        selector: &str,
        local_machine_id: Option<&str>,
    ) -> Result<ResolvedWorkspaceSelection, OrbitError> {
        let (workspace, checkout, local_root) =
            resolve_cli_workspace_binding(registry, selector, local_machine_id)?;
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
        // One identity read serves both the task-prefix projection and the
        // automation machine identity; a long-lived host opens enough runtimes
        // for a second resolution of the same global config to be pure overhead.
        let identity = inspect_machine_identity(global_root)?;
        Self::open_registered_checkout_with_identity(
            global_root,
            workspace,
            checkout,
            host_lifetime,
            &identity,
        )
    }

    /// [`Self::open_registered_checkout_for`] from a machine-identity
    /// classification the caller already read to resolve the selector
    /// [DANI-10371].
    fn open_registered_checkout_with_identity(
        global_root: &Path,
        workspace: &Workspace,
        checkout: &WorkspaceCheckout,
        host_lifetime: HostLifetime,
        identity: &MachineIdentityState,
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
        identity: &MachineIdentityState,
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
        let identity = inspect_machine_identity(global_root)?;
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
        identity: &MachineIdentityState,
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
        let identity = inspect_machine_identity(global_root)?;
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
        let identity = inspect_machine_identity(&global_root)?;
        let registry = load_registry_for_selector_resolution(
            &workspace_registry::registry_path_for(&global_root),
            &identity,
        )?;
        match resolve_cli_workspace_target(&registry, runtime, &selector, identity.id())? {
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
