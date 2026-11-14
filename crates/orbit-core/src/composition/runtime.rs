//! Runtime composition: root resolution, runtime construction and configuration
//! initialization for [`OrbitRuntime`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_common::fs::generation::{
    Access, GenerationGuard, ParticipantRole, process_handover, process_participation,
};
use orbit_config::{ConfigRoots, ResolvedConfig};
use orbit_store::Store;
use orbit_store::compose::compiled_compatibility;
use orbit_store::compose::global_policy_def_store;

use super::application_port::APPLICATION_PORT;
use crate::application::task::legacy_decisions::record_typed_decisions_boundary;
use crate::application::workspace_product_profile::ProductProfile;
use crate::bootstrap::global_defaults::global_defaults_are_current;
use crate::bootstrap::init;
use crate::bootstrap::policy::seed_default_policies;
use crate::bootstrap::task_migration::apply_configured_id_start;
use crate::bootstrap::workspace_authority::{
    admit_workspace_layout_authority, verify_workspace_layout_authority,
};
#[cfg(target_os = "macos")]
use crate::runtime::run_input::managed_dispatch_context_from_env;
use crate::runtime::run_input::managed_run_context_from_env;
use crate::runtime::{
    HostLifetime, OrbitRuntime, OrbitRuntimeRoots, ResolvedOrbitRoots, RuntimeConfigLoader,
    RuntimeKernel, WorkspaceRootHint, WorkspaceRuntimeBinding, resolve_bootstrap_roots,
    resolve_bootstrap_roots_with_hint, resolve_global_root, resolve_initialize_roots,
    resolve_initialize_roots_with_hint,
};

impl OrbitRuntime {
    pub fn initialize() -> Result<Self, OrbitError> {
        Self::initialize_with_root_override(None)
    }

    pub fn initialize_with_root_override(root_override: Option<&Path>) -> Result<Self, OrbitError> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let roots = Self::resolve_roots_for_cwd(&cwd, root_override)?;
        Self::initialize_from_resolved_roots(roots, None)
    }

    pub fn initialize_from_resolved_roots(
        roots: OrbitRuntimeRoots,
        binding: Option<WorkspaceRuntimeBinding>,
    ) -> Result<Self, OrbitError> {
        Self::initialize_from_resolved_roots_with_task_prefix(roots, binding, None)
    }

    /// [`Self::initialize_from_resolved_roots`] that projects `task_prefix`
    /// onto the task-registry handle this open creates.
    ///
    /// `None` leaves the allocator prefix untouched. The runtime kernel does
    /// not load machine config; composition supplies the prefix it already
    /// classified from the narrow machine-identity read.
    pub fn initialize_from_resolved_roots_with_task_prefix(
        roots: OrbitRuntimeRoots,
        binding: Option<WorkspaceRuntimeBinding>,
        task_prefix: Option<&str>,
    ) -> Result<Self, OrbitError> {
        ProductProfile::Orbit.validate_roots(&[
            &roots.global_root,
            &roots.shared_root,
            &roots.local_root,
        ])?;
        // Library consumers must also refuse before bootstrap can migrate or
        // reconcile resources under a participating persistent CLI/MCP client.
        let generation = pin_executable_generation(&roots.global_root, false)?;
        if !generation.joined_foreign_generation() {
            // The workspace's clients may participate in another authority:
            // refuse before bootstrap writes under them [ORB-20244].
            verify_workspace_layout_authority(&roots.global_root, &roots.shared_root)?;
        }
        init::ensure_orbit_root_initialized(&roots.global_root, &roots.shared_root)?;
        build_runtime(
            &roots.global_root,
            &roots.shared_root,
            &roots.local_root,
            binding,
            true,
            HostLifetime::ShortLived,
            false,
            task_prefix,
        )
    }

    /// Open an existing workspace for an observation-only command without
    /// reconciling stale job runs as a side effect of runtime construction.
    pub fn initialize_from_resolved_roots_read_only(
        roots: OrbitRuntimeRoots,
        binding: Option<WorkspaceRuntimeBinding>,
    ) -> Result<Self, OrbitError> {
        build_runtime(
            &roots.global_root,
            &roots.shared_root,
            &roots.local_root,
            binding,
            false,
            HostLifetime::ShortLived,
            true,
            None,
        )
    }

    pub fn resolve_roots_for_cwd(
        cwd: &Path,
        root_override: Option<&Path>,
    ) -> Result<OrbitRuntimeRoots, OrbitError> {
        roots_from_resolved(
            resolve_initialize_roots(cwd, root_override)?,
            has_explicit_root_override(root_override),
        )
    }

    pub fn resolve_roots_for_cwd_with_hint(
        cwd: &Path,
        root_override: Option<&Path>,
        hint: Option<&WorkspaceRootHint>,
    ) -> Result<OrbitRuntimeRoots, OrbitError> {
        roots_from_resolved(
            resolve_initialize_roots_with_hint(cwd, root_override, hint)?,
            has_explicit_root_override(root_override),
        )
    }

    pub fn resolve_bootstrap_roots_for_cwd(
        cwd: &Path,
        root_override: Option<&Path>,
    ) -> Result<OrbitRuntimeRoots, OrbitError> {
        roots_from_resolved(
            resolve_bootstrap_roots(cwd, root_override)?,
            has_explicit_root_override(root_override),
        )
    }

    pub fn resolve_bootstrap_roots_for_cwd_with_hint(
        cwd: &Path,
        root_override: Option<&Path>,
        hint: Option<&WorkspaceRootHint>,
    ) -> Result<OrbitRuntimeRoots, OrbitError> {
        roots_from_resolved(
            resolve_bootstrap_roots_with_hint(cwd, root_override, hint)?,
            has_explicit_root_override(root_override),
        )
    }

    pub fn from_roots(global_root: &Path, workspace_root: &Path) -> Result<Self, OrbitError> {
        Self::from_resolved_roots(global_root, workspace_root, workspace_root)
    }

    pub fn from_roots_with_binding(
        global_root: &Path,
        workspace_root: &Path,
        binding: WorkspaceRuntimeBinding,
    ) -> Result<Self, OrbitError> {
        Self::from_roots_with_binding_for(
            global_root,
            workspace_root,
            binding,
            HostLifetime::ShortLived,
        )
    }

    pub fn from_roots_with_binding_for(
        global_root: &Path,
        workspace_root: &Path,
        binding: WorkspaceRuntimeBinding,
        host_lifetime: HostLifetime,
    ) -> Result<Self, OrbitError> {
        Self::from_resolved_roots_with_binding_for(
            global_root,
            workspace_root,
            workspace_root,
            binding,
            host_lifetime,
        )
    }

    pub fn from_resolved_roots(
        global_root: &Path,
        shared_root: &Path,
        local_root: &Path,
    ) -> Result<Self, OrbitError> {
        Self::from_resolved_roots_with_task_prefix(global_root, shared_root, local_root, None)
    }

    /// [`Self::from_resolved_roots`] that projects `task_prefix` onto the
    /// task-registry handle this open creates. See
    /// [`Self::initialize_from_resolved_roots_with_task_prefix`].
    pub fn from_resolved_roots_with_task_prefix(
        global_root: &Path,
        shared_root: &Path,
        local_root: &Path,
        task_prefix: Option<&str>,
    ) -> Result<Self, OrbitError> {
        build_runtime(
            global_root,
            shared_root,
            local_root,
            None,
            true,
            HostLifetime::ShortLived,
            false,
            task_prefix,
        )
    }

    pub fn from_resolved_roots_with_binding(
        global_root: &Path,
        shared_root: &Path,
        local_root: &Path,
        binding: WorkspaceRuntimeBinding,
    ) -> Result<Self, OrbitError> {
        Self::from_resolved_roots_with_binding_for(
            global_root,
            shared_root,
            local_root,
            binding,
            HostLifetime::ShortLived,
        )
    }

    /// Open a runtime for the constructing host's lifetime.
    pub fn from_resolved_roots_with_binding_for(
        global_root: &Path,
        shared_root: &Path,
        local_root: &Path,
        binding: WorkspaceRuntimeBinding,
        host_lifetime: HostLifetime,
    ) -> Result<Self, OrbitError> {
        Self::from_resolved_roots_with_binding_for_task_prefix(
            global_root,
            shared_root,
            local_root,
            binding,
            host_lifetime,
            None,
        )
    }

    /// [`Self::from_resolved_roots_with_binding_for`] that projects
    /// `task_prefix` onto the task-registry handle this open creates. See
    /// [`Self::initialize_from_resolved_roots_with_task_prefix`].
    pub fn from_resolved_roots_with_binding_for_task_prefix(
        global_root: &Path,
        shared_root: &Path,
        local_root: &Path,
        binding: WorkspaceRuntimeBinding,
        host_lifetime: HostLifetime,
        task_prefix: Option<&str>,
    ) -> Result<Self, OrbitError> {
        build_runtime(
            global_root,
            shared_root,
            local_root,
            Some(binding),
            true,
            host_lifetime,
            false,
            task_prefix,
        )
    }

    pub fn from_resolved_roots_read_only_with_binding(
        global_root: &Path,
        shared_root: &Path,
        local_root: &Path,
        binding: WorkspaceRuntimeBinding,
    ) -> Result<Self, OrbitError> {
        build_runtime(
            global_root,
            shared_root,
            local_root,
            Some(binding),
            false,
            HostLifetime::ShortLived,
            true,
            None,
        )
    }

    /// Open existing state for host routine inspection without migrations,
    /// legacy imports, task-partition repair or stale-run reconciliation.
    pub fn from_resolved_roots_observational_with_binding(
        global_root: &Path,
        shared_root: &Path,
        binding: WorkspaceRuntimeBinding,
    ) -> Result<Self, OrbitError> {
        ProductProfile::Orbit.validate_roots(&[global_root, shared_root])?;
        let _generation = pin_executable_generation(global_root, true)?;
        let layout_report = observe_or_upgrade_layout(shared_root, true)?;
        let config = ResolvedConfig::load(&ConfigRoots::new(global_root, shared_root))?;
        RuntimeKernel::build_from_resolved_config_write_free(
            &OrbitRuntimeRoots {
                global_root: global_root.to_path_buf(),
                shared_root: shared_root.to_path_buf(),
                local_root: shared_root.to_path_buf(),
            },
            Some(binding),
            &config,
            layout_report,
            HostLifetime::ShortLived,
            runtime_config_loader(global_root, shared_root),
            &APPLICATION_PORT,
        )
        .map(Self::from_kernel)
    }

    pub fn in_memory() -> Result<Self, OrbitError> {
        let temp_dir = tempfile::Builder::new()
            .prefix("orbit-in-memory-")
            .tempdir()
            .map_err(|error| OrbitError::Io(error.to_string()))?;
        let data_root = temp_dir.path().to_path_buf();
        let workspace_root = data_root.join(".orbit");
        let runtime_config = prepare_resolved_config(&data_root, &workspace_root)?;
        RuntimeKernel::build_in_memory_from_resolved_config(
            &data_root,
            &workspace_root,
            &runtime_config,
            temp_dir,
            runtime_config_loader(&data_root, &workspace_root),
            &APPLICATION_PORT,
        )
        .map(Self::from_kernel)
    }
}

/// Join `root`'s generation before bootstrap, in the role and with the
/// handover capability this process already joined with (a plain command
/// otherwise). See [`pin_executable_generation_as`].
pub fn pin_executable_generation(
    root: &Path,
    read_only: bool,
) -> Result<GenerationGuard, OrbitError> {
    let role = process_participation().map_or(ParticipantRole::Command, |(_, role)| role);
    pin_executable_generation_as(root, read_only, role, process_handover())
}

/// Join `root`'s generation before bootstrap under
/// `compatibility-generation-v2`: any build whose store schema, workspace
/// layout and feature schemas are compatible with the live participants is
/// admitted, whatever its executable digest. A read-only caller that meets a
/// v1-owned generation still joins it when the compiled store schema equals
/// the store's current schema.
///
/// `handover` is the resume capability this process hands over with when a
/// candidate is renamed over its executable, if it can; an updater admitting
/// that candidate then admits beside it.
pub fn pin_executable_generation_as(
    root: &Path,
    read_only: bool,
    role: ParticipantRole,
    handover: Option<&str>,
) -> Result<GenerationGuard, OrbitError> {
    let access = if read_only {
        Access::ReadOnly
    } else {
        Access::Write
    };
    GenerationGuard::for_process(
        root,
        &compiled_compatibility(),
        role,
        handover,
        access,
        crate::runtime::run_input::managed_run_context_from_env(),
        || Store::open_read_only(&root.join("orbit.db"))?.schema_version(),
    )
}

#[allow(clippy::too_many_arguments)]
fn build_runtime(
    global_root: &Path,
    shared_root: &Path,
    local_root: &Path,
    binding: Option<WorkspaceRuntimeBinding>,
    reconcile_stale_runs: bool,
    host_lifetime: HostLifetime,
    read_only: bool,
    task_prefix: Option<&str>,
) -> Result<OrbitRuntime, OrbitError> {
    ProductProfile::Orbit.validate_roots(&[global_root, shared_root, local_root])?;
    let generation = pin_executable_generation(global_root, read_only)?;
    let write_free = generation.joined_foreign_generation();
    let authority = if write_free {
        None
    } else {
        Some(admit_workspace_layout_authority(global_root, shared_root)?)
    };
    let layout_report = observe_or_upgrade_layout(shared_root, write_free)?;
    // The admission recorded the authority before the layout changed and
    // fenced other first adoptions until now.
    drop(authority);
    let runtime_config = if write_free {
        ResolvedConfig::load(&ConfigRoots::new(global_root, shared_root))?
    } else {
        prepare_resolved_config(global_root, shared_root)?
    };
    let config_loader = runtime_config_loader(global_root, shared_root);
    if !write_free && !read_only {
        init::refresh_workspace_catalog_copies_on_open(global_root, shared_root);
        // Before this runtime can write a task record, so text-form decisions
        // an older binary wrote stay legacy and text written from now on does
        // not.
        if let Err(error) = record_typed_decisions_boundary(global_root) {
            if error.is_readonly_or_access_failure() {
                tracing::warn!(
                    target: "orbit.core.bootstrap",
                    root = %global_root.display(),
                    error = %error,
                    "skipped incidental typed decisions boundary persistence"
                );
            } else {
                return Err(error);
            }
        }
    }
    let roots = OrbitRuntimeRoots {
        global_root: global_root.to_path_buf(),
        shared_root: shared_root.to_path_buf(),
        local_root: local_root.to_path_buf(),
    };
    let kernel = if write_free {
        RuntimeKernel::build_from_resolved_config_write_free(
            &roots,
            binding,
            &runtime_config,
            layout_report,
            host_lifetime,
            config_loader,
            &APPLICATION_PORT,
        )?
    } else {
        RuntimeKernel::build_from_resolved_config(
            &roots,
            binding,
            &runtime_config,
            layout_report,
            host_lifetime,
            read_only,
            config_loader,
            &APPLICATION_PORT,
            if read_only { None } else { task_prefix },
        )?
    };
    let runtime = OrbitRuntime::from_kernel(kernel);
    let _generation = generation;
    if reconcile_stale_runs && !write_free && !managed_run_context_from_env() {
        runtime.reconcile_stale_job_runs_on_open();
    }
    if !write_free && !read_only && !managed_run_context_from_env() {
        // Records an earlier release wrote with pre-family tool names
        // converge once per root, with no manual step [ORB-23552].
        runtime.migrate_pre_family_tool_names_on_open();
    }
    Ok(runtime)
}

fn runtime_config_loader(global_root: &Path, shared_root: &Path) -> RuntimeConfigLoader {
    let global_root = global_root.to_path_buf();
    let shared_root = shared_root.to_path_buf();
    Arc::new(move || ResolvedConfig::load(&ConfigRoots::new(&global_root, &shared_root)))
}

fn observe_or_upgrade_layout(
    shared_root: &Path,
    write_free: bool,
) -> Result<orbit_store::workflow::layout::LayoutUpgradeReport, OrbitError> {
    if write_free {
        let current = orbit_store::workflow::layout::current_layout_version(shared_root)?;
        if current < orbit_store::workflow::layout::SUPPORTED_LAYOUT_VERSION {
            return Ok(orbit_store::workflow::layout::LayoutUpgradeReport {
                from_version: current,
                to_version: current,
                applied: Vec::new(),
                forward_compatible: None,
            });
        }
    }
    match orbit_store::workflow::layout::upgrade_workspace_layout(shared_root) {
        Ok(report) => Ok(report),
        Err(error) if error.is_readonly_or_access_failure() => {
            tracing::warn!(
                target: "orbit.core.bootstrap",
                root = %shared_root.display(),
                error = %error,
                "skipped incidental workspace layout persistence"
            );
            Ok(orbit_store::workflow::layout::LayoutUpgradeReport::default())
        }
        Err(error) => Err(error),
    }
}

fn prepare_resolved_config(
    global_root: &Path,
    workspace_root: &Path,
) -> Result<ResolvedConfig, OrbitError> {
    let resolved = ResolvedConfig::load(&ConfigRoots::new(global_root, workspace_root))?;
    if let Some(start) = resolved.tasks_id_start
        && let Err(error) = apply_configured_id_start(global_root, start)
    {
        if error.is_readonly_or_access_failure() {
            tracing::warn!(
                target: "orbit.core.bootstrap",
                root = %global_root.display(),
                error = %error,
                "skipped incidental task allocator bootstrap persistence"
            );
        } else {
            return Err(error);
        }
    }
    // Not every runtime open bootstraps its global root: opening a registered
    // checkout or an observation-only runtime goes straight to composition, so
    // the shipped default policy is seeded here. `policy_dir` is derived from
    // the global root and cannot be relocated by config, so the same stamp that
    // lets bootstrap skip reconciliation settles this seed too — a root already
    // reconciled by this binary carries the policy.
    if !global_defaults_are_current(global_root) {
        let global_policy_store = global_policy_def_store(resolved.persistence.policy_dir.clone());
        if let Err(error) = seed_default_policies(global_policy_store.as_ref(), false) {
            if error.is_readonly_or_access_failure() {
                tracing::warn!(
                    target: "orbit.core.bootstrap",
                    root = %global_root.display(),
                    error = %error,
                    "skipped incidental default-policy persistence"
                );
            } else {
                return Err(error);
            }
        }
    }
    Ok(resolved)
}

fn roots_from_resolved(
    resolved: ResolvedOrbitRoots,
    pin_global_to_shared: bool,
) -> Result<OrbitRuntimeRoots, OrbitError> {
    let global_root = if pin_global_to_shared {
        resolved.shared_root.clone()
    } else {
        resolve_global_root()?
    };
    Ok(OrbitRuntimeRoots {
        global_root,
        shared_root: resolved.shared_root,
        local_root: resolved.local_root,
    })
}

fn has_explicit_root_override(root_override: Option<&Path>) -> bool {
    if root_override.is_some() {
        return true;
    }
    let env_override = std::env::var("ORBIT_ROOT").is_ok_and(|value| !value.trim().is_empty());
    #[cfg(target_os = "macos")]
    if env_override
        && managed_dispatch_context_from_env()
        && std::env::var("ORBIT_REGISTRY_ROOT").is_ok_and(|value| !value.trim().is_empty())
    {
        // The sandbox may not create a generation record under the selected
        // workspace `.orbit`. Keep global stores and their generation pin on
        // the host registry while ORBIT_ROOT selects shared/local state.
        return false;
    }
    env_override
}
