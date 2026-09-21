//! Composition root joining resolved configuration, the runtime kernel, and adapters.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::generation::GenerationGuard;
use orbit_config::{ConfigRoots, ResolvedConfig};
use orbit_store::Store;
use orbit_store::compose::global_policy_def_store;
use orbit_store::maintenance::migration::SUPPORTED_SCHEMA_VERSION;

use crate::bootstrap::global_defaults::global_defaults_are_current;
use crate::bootstrap::init::ensure_orbit_root_initialized;
use crate::bootstrap::policy::seed_default_policies;
use crate::bootstrap::product_profile::ProductProfile;
use crate::bootstrap::task_migration::apply_configured_id_start;
use crate::runtime::run_input::managed_run_context_from_env;
use crate::runtime::{
    HostLifetime, OrbitRuntime, OrbitRuntimeRoots, ResolvedOrbitRoots, WorkspaceRootHint,
    WorkspaceRuntimeBinding, resolve_bootstrap_roots, resolve_bootstrap_roots_with_hint,
    resolve_global_root, resolve_initialize_roots, resolve_initialize_roots_with_hint,
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
        ProductProfile::Orbit.validate_roots(&[
            &roots.global_root,
            &roots.shared_root,
            &roots.local_root,
        ])?;
        // Library consumers must also refuse before bootstrap can migrate or
        // reconcile resources under a participating persistent CLI/MCP client.
        let _generation = pin_executable_generation(&roots.global_root, false)?;
        ensure_orbit_root_initialized(&roots.global_root, &roots.shared_root)?;
        build_runtime(
            &roots.global_root,
            &roots.shared_root,
            &roots.local_root,
            binding,
            true,
            HostLifetime::ShortLived,
            false,
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
        build_runtime(
            global_root,
            shared_root,
            local_root,
            None,
            true,
            HostLifetime::ShortLived,
            false,
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
        build_runtime(
            global_root,
            shared_root,
            local_root,
            Some(binding),
            true,
            host_lifetime,
            false,
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
        )
    }

    /// Bootstrap feasibility fixture only. Does not establish admission or
    /// re-entry isolation, so must never be exposed by a production executable.
    #[cfg(test)]
    pub(crate) fn initialize_research_fixture(
        roots: OrbitRuntimeRoots,
    ) -> Result<Self, OrbitError> {
        crate::bootstrap::product_profile::initialize_research_catalog(&roots)?;
        let resolved =
            ResolvedConfig::load(&ConfigRoots::new(&roots.global_root, &roots.shared_root))?;
        Self::build_from_resolved_config(
            &roots.global_root,
            &roots.shared_root,
            &roots.local_root,
            None,
            &resolved,
            orbit_store::workflow::layout::LayoutUpgradeReport::default(),
            HostLifetime::ShortLived,
        )
    }

    pub fn in_memory() -> Result<Self, OrbitError> {
        let temp_dir = tempfile::Builder::new()
            .prefix("orbit-in-memory-")
            .tempdir()
            .map_err(|error| OrbitError::Io(error.to_string()))?;
        let data_root = temp_dir.path().to_path_buf();
        let workspace_root = data_root.join(".orbit");
        let runtime_config = prepare_resolved_config(&data_root, &workspace_root)?;
        Self::build_in_memory_from_resolved_config(
            &data_root,
            &workspace_root,
            &runtime_config,
            temp_dir,
        )
    }
}

/// Pin this process against `root` before bootstrap. Read-only callers may
/// join a live generation without rewriting `.generation.lock` when the
/// compiled store schema equals the store's current schema.
pub fn pin_executable_generation(
    root: &Path,
    read_only: bool,
) -> Result<GenerationGuard, OrbitError> {
    if read_only {
        GenerationGuard::for_process_read_only(root, SUPPORTED_SCHEMA_VERSION, || {
            Store::open_read_only(&root.join("orbit.db"))?.schema_version()
        })
    } else {
        GenerationGuard::for_process(root)
    }
}

fn build_runtime(
    global_root: &Path,
    shared_root: &Path,
    local_root: &Path,
    binding: Option<WorkspaceRuntimeBinding>,
    reconcile_stale_runs: bool,
    host_lifetime: HostLifetime,
    read_only: bool,
) -> Result<OrbitRuntime, OrbitError> {
    ProductProfile::Orbit.validate_roots(&[global_root, shared_root, local_root])?;
    let generation = pin_executable_generation(global_root, read_only)?;
    let write_free = generation.joined_foreign_generation();
    let layout_report = observe_or_upgrade_layout(shared_root, write_free)?;
    let runtime_config = if write_free {
        ResolvedConfig::load(&ConfigRoots::new(global_root, shared_root))?
    } else {
        prepare_resolved_config(global_root, shared_root)?
    };
    let runtime = if write_free {
        OrbitRuntime::build_from_resolved_config_write_free(
            global_root,
            shared_root,
            local_root,
            binding,
            &runtime_config,
            layout_report,
            host_lifetime,
        )?
    } else {
        OrbitRuntime::build_from_resolved_config(
            global_root,
            shared_root,
            local_root,
            binding,
            &runtime_config,
            layout_report,
            host_lifetime,
        )?
    };
    let _generation = generation;
    if reconcile_stale_runs && !write_free && !managed_run_context_from_env() {
        runtime.reconcile_stale_job_runs_on_open();
    }
    Ok(runtime)
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
    root_override.is_some()
        || std::env::var("ORBIT_ROOT").is_ok_and(|value| !value.trim().is_empty())
}
